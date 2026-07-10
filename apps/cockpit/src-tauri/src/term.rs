//! Real interactive terminals for the session UI, backed by portable-pty
//! (ConPTY on Windows). Output streams to the webview over the
//! `term-output-msg` event; input/resize/close are commands.

use crate::engine::EngineClient;
use crate::error::CmdError;
use portable_pty::{CommandBuilder, MasterPty, PtySize};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::io::{Read, Write};
use std::sync::{Arc, Mutex};
use tauri::State;
use tauri_specta::Event;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct TermOutputMsg {
    pub id: String,
    /// UTF-8 chunk (lossy) of PTY output.
    pub data: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct TermExitMsg {
    pub id: String,
}

struct TermHandle {
    session_pk: String,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn Write + Send>,
    /// Kills the shell on close — EOF alone can leave it orphaned (holding its
    /// cwd, e.g. a session worktree pending deletion).
    killer: Box<dyn portable_pty::ChildKiller + Send + Sync>,
}

#[derive(Default)]
pub struct UiTerms(Mutex<HashMap<String, TermHandle>>);

fn default_shell() -> (String, Vec<String>) {
    if cfg!(windows) {
        // Prefer PowerShell 7 when present.
        if ryuzi_core::process_util::find_on_path("pwsh").is_some() {
            ("pwsh".into(), vec!["-NoLogo".into()])
        } else {
            ("powershell".into(), vec!["-NoLogo".into()])
        }
    } else {
        (
            std::env::var("SHELL").unwrap_or_else(|_| "/bin/bash".into()),
            vec![],
        )
    }
}

/// Open a shell in the session's worktree (or the project workdir).
#[tauri::command]
#[specta::specta]
pub async fn term_open(
    app: tauri::AppHandle,
    engine: State<'_, Arc<EngineClient>>,
    terms: State<'_, Arc<UiTerms>>,
    session_pk: String,
    cols: u16,
    rows: u16,
) -> R<String> {
    let cwd: String = engine
        .rpc(
            "session_workdir",
            serde_json::json!({ "session_pk": session_pk.clone() }),
        )
        .await?;

    let pty_system = portable_pty::native_pty_system();
    let pair = pty_system
        .openpty(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| CmdError {
            message: format!("openpty failed: {e}"),
        })?;

    let (shell, args) = default_shell();
    let mut cmd = CommandBuilder::new(shell);
    cmd.args(args);
    cmd.cwd(&cwd);
    let mut child = pair.slave.spawn_command(cmd).map_err(|e| CmdError {
        message: format!("shell spawn failed: {e}"),
    })?;
    let killer = child.clone_killer();
    drop(pair.slave);

    let id = format!("t-{}", &ryuzi_core::paths::new_id()[..8]);
    let mut reader = pair.master.try_clone_reader().map_err(|e| CmdError {
        message: format!("pty reader failed: {e}"),
    })?;
    let writer = pair.master.take_writer().map_err(|e| CmdError {
        message: format!("pty writer failed: {e}"),
    })?;

    terms.0.lock().unwrap().insert(
        id.clone(),
        TermHandle {
            session_pk: session_pk.clone(),
            master: pair.master,
            writer,
            killer,
        },
    );

    // Reader thread: stream chunks to the webview until the PTY closes.
    let app_reader = app.clone();
    let id_reader = id.clone();
    std::thread::spawn(move || {
        let mut buf = [0u8; 8192];
        loop {
            match reader.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    let data = String::from_utf8_lossy(&buf[..n]).into_owned();
                    let _ = TermOutputMsg {
                        id: id_reader.clone(),
                        data,
                    }
                    .emit(&app_reader);
                }
            }
        }
        let _ = TermExitMsg {
            id: id_reader.clone(),
        }
        .emit(&app_reader);
    });

    // Reaper thread: don't leave zombie shells behind.
    std::thread::spawn(move || {
        let _ = child.wait();
    });

    Ok(id)
}

#[tauri::command]
#[specta::specta]
pub fn term_input(terms: State<'_, Arc<UiTerms>>, id: String, data: String) -> R<()> {
    let mut map = terms.0.lock().unwrap();
    let handle = map.get_mut(&id).ok_or_else(|| CmdError {
        message: "terminal closed".into(),
    })?;
    handle
        .writer
        .write_all(data.as_bytes())
        .map_err(|e| CmdError {
            message: format!("write failed: {e}"),
        })?;
    let _ = handle.writer.flush();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub fn term_resize(terms: State<'_, Arc<UiTerms>>, id: String, cols: u16, rows: u16) -> R<()> {
    let map = terms.0.lock().unwrap();
    let handle = map.get(&id).ok_or_else(|| CmdError {
        message: "terminal closed".into(),
    })?;
    handle
        .master
        .resize(PtySize {
            rows,
            cols,
            pixel_width: 0,
            pixel_height: 0,
        })
        .map_err(|e| CmdError {
            message: format!("resize failed: {e}"),
        })
}

#[tauri::command]
#[specta::specta]
pub fn term_close(terms: State<'_, Arc<UiTerms>>, id: String) -> R<()> {
    // Kill the shell explicitly, then drop the handle to close the PTY —
    // relying on EOF alone can leave the shell orphaned with its cwd held.
    if let Some(mut handle) = terms.0.lock().unwrap().remove(&id) {
        let _ = handle.killer.kill();
    }
    Ok(())
}

/// Kill every shell opened for a session — the archive/end flow runs this
/// BEFORE worktree teardown, or the shells' cwd handles block the directory
/// removal on Windows.
#[tauri::command]
#[specta::specta]
pub fn term_close_session(terms: State<'_, Arc<UiTerms>>, session_pk: String) -> R<()> {
    let mut map = terms.0.lock().unwrap();
    let ids: Vec<String> = map
        .iter()
        .filter(|(_, h)| h.session_pk == session_pk)
        .map(|(id, _)| id.clone())
        .collect();
    for id in ids {
        if let Some(mut handle) = map.remove(&id) {
            let _ = handle.killer.kill();
        }
    }
    Ok(())
}
