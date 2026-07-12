//! "Open in…" targets: detect installed editors/terminals/file managers once
//! per app run and launch them detached on the session workdir.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use tauri::State;

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct OpenTarget {
    pub id: String,
    pub name: String,
}

/// How to launch one resolved target on a directory.
#[derive(Debug, Clone)]
struct ResolvedTarget {
    id: &'static str,
    name: &'static str,
    /// Absolute program path, or a bare name the OS resolves itself
    /// (builtins like explorer/open/xdg-open).
    program: PathBuf,
    /// Arguments with `{dir}` substituted by the target directory.
    args: &'static [&'static str],
    /// True when the target intentionally opens an interactive terminal.
    console: bool,
}

/// PATH lookup honoring PATHEXT on Windows.
fn find_in_path(program: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    let exts: Vec<String> = if cfg!(windows) {
        std::env::var("PATHEXT")
            .unwrap_or_else(|_| ".EXE;.CMD;.BAT;.COM".into())
            .split(';')
            .map(|e| e.to_ascii_lowercase())
            .collect()
    } else {
        vec![String::new()]
    };
    for dir in std::env::split_paths(&path_var) {
        for ext in &exts {
            let cand = dir.join(format!("{program}{ext}"));
            if cand.is_file() {
                return Some(cand);
            }
        }
    }
    None
}

fn local_app_data() -> PathBuf {
    PathBuf::from(std::env::var("LOCALAPPDATA").unwrap_or_default())
}

/// Resolve the launchable targets for this OS. `lookup` and `exists` are
/// injected so tests can fake the filesystem.
fn resolve_targets(
    lookup: &dyn Fn(&str) -> Option<PathBuf>,
    exists: &dyn Fn(&PathBuf) -> bool,
) -> Vec<ResolvedTarget> {
    fn builtin(
        id: &'static str,
        name: &'static str,
        program: &str,
        args: &'static [&'static str],
    ) -> ResolvedTarget {
        ResolvedTarget {
            id,
            name,
            program: PathBuf::from(program),
            args,
            console: false,
        }
    }
    let mut out = Vec::new();

    if cfg!(target_os = "windows") {
        out.push(builtin(
            "explorer",
            "File Explorer",
            "explorer.exe",
            &["{dir}"],
        ));
        if let Some(p) = lookup("wt") {
            out.push(ResolvedTarget {
                id: "terminal",
                name: "Terminal",
                program: p,
                args: &["-d", "{dir}"],
                console: true,
            });
        }
        let code_fallback = local_app_data().join("Programs/Microsoft VS Code/bin/code.cmd");
        if let Some(p) =
            lookup("code").or_else(|| exists(&code_fallback).then(|| code_fallback.clone()))
        {
            out.push(ResolvedTarget {
                id: "vscode",
                name: "Visual Studio Code",
                program: p,
                args: &["{dir}"],
                console: false,
            });
        }
        let cursor_fallback = local_app_data().join("Programs/cursor/resources/app/bin/cursor.cmd");
        if let Some(p) =
            lookup("cursor").or_else(|| exists(&cursor_fallback).then(|| cursor_fallback.clone()))
        {
            out.push(ResolvedTarget {
                id: "cursor",
                name: "Cursor",
                program: p,
                args: &["{dir}"],
                console: false,
            });
        }
        for base in [
            "C:/Program Files/Git/git-bash.exe",
            "C:/Program Files (x86)/Git/git-bash.exe",
        ] {
            let p = PathBuf::from(base);
            if exists(&p) {
                out.push(ResolvedTarget {
                    id: "git-bash",
                    name: "Git Bash",
                    program: p,
                    args: &["--cd={dir}"],
                    console: true,
                });
                break;
            }
        }
        if let Some(p) = lookup("wsl") {
            out.push(ResolvedTarget {
                id: "wsl",
                name: "WSL",
                program: p,
                args: &["--cd", "{dir}"],
                console: true,
            });
        }
    } else if cfg!(target_os = "macos") {
        out.push(builtin("finder", "Finder", "open", &["{dir}"]));
        out.push(ResolvedTarget {
            id: "terminal",
            name: "Terminal",
            program: PathBuf::from("open"),
            args: &["-a", "Terminal", "{dir}"],
            console: true,
        });
        if exists(&PathBuf::from("/Applications/Visual Studio Code.app"))
            || lookup("code").is_some()
        {
            out.push(builtin(
                "vscode",
                "Visual Studio Code",
                "open",
                &["-a", "Visual Studio Code", "{dir}"],
            ));
        }
        if exists(&PathBuf::from("/Applications/Cursor.app")) || lookup("cursor").is_some() {
            out.push(builtin(
                "cursor",
                "Cursor",
                "open",
                &["-a", "Cursor", "{dir}"],
            ));
        }
    } else {
        out.push(builtin("files", "Files", "xdg-open", &["{dir}"]));
        if let Some(p) = lookup("code") {
            out.push(ResolvedTarget {
                id: "vscode",
                name: "Visual Studio Code",
                program: p,
                args: &["{dir}"],
                console: false,
            });
        }
        if let Some(p) = lookup("cursor") {
            out.push(ResolvedTarget {
                id: "cursor",
                name: "Cursor",
                program: p,
                args: &["{dir}"],
                console: false,
            });
        }
        for term in ["x-terminal-emulator", "gnome-terminal", "konsole"] {
            if let Some(p) = lookup(term) {
                out.push(ResolvedTarget {
                    id: "terminal",
                    name: "Terminal",
                    program: p,
                    args: &["--working-directory={dir}"],
                    console: true,
                });
                break;
            }
        }
    }
    out
}

/// Detection runs once per app run (installs rarely change mid-session).
fn targets() -> &'static Vec<ResolvedTarget> {
    static TARGETS: OnceLock<Vec<ResolvedTarget>> = OnceLock::new();
    TARGETS.get_or_init(|| resolve_targets(&find_in_path, &|p| p.is_file() || p.is_dir()))
}

#[tauri::command]
#[specta::specta]
pub fn list_open_targets() -> Vec<OpenTarget> {
    targets()
        .iter()
        .map(|t| OpenTarget {
            id: t.id.to_string(),
            name: t.name.to_string(),
        })
        .collect()
}

#[tauri::command]
#[specta::specta]
pub async fn open_in(
    engine: Engine<'_>,
    runner_id: Option<String>,
    session_pk: String,
    target_id: String,
) -> R<()> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    let dir: String = client
        .rpc(
            "session_workdir",
            serde_json::json!({ "session_pk": session_pk }),
        )
        .await?;
    let target = targets()
        .iter()
        .find(|t| t.id == target_id)
        .cloned()
        .ok_or_else(|| CmdError {
            message: format!("unknown open target: {target_id}"),
        })?;
    let args: Vec<String> = target
        .args
        .iter()
        .map(|a| a.replace("{dir}", &dir))
        .collect();
    // Detached fire-and-forget: the spawned app outlives any turn. Hide the
    // console for file managers and editor shims; terminal targets stay visible.
    let mut cmd = std::process::Command::new(&target.program);
    cmd.args(&args);
    if !target.console {
        ryuzi_core::process_util::no_window_std(&mut cmd);
    }
    cmd.spawn().map_err(|e| CmdError {
        message: format!("couldn't launch {}: {e}", target.name),
    })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolve_targets_includes_only_detected_apps() {
        let lookup = |name: &str| -> Option<PathBuf> {
            (name == "code").then(|| PathBuf::from("/fake/bin/code"))
        };
        let exists = |_: &PathBuf| false;
        let targets = resolve_targets(&lookup, &exists);
        // Every OS branch has a builtin file manager plus the faked VS Code.
        assert!(targets.iter().any(|t| t.id == "vscode" && !t.console));
        assert!(targets
            .iter()
            .any(|t| matches!(t.id, "explorer" | "finder" | "files")));
        assert!(!targets.iter().any(|t| t.id == "cursor"));
    }

    #[test]
    fn open_target_args_substitute_the_directory() {
        let t = ResolvedTarget {
            id: "x",
            name: "X",
            program: PathBuf::from("x"),
            args: &["--cd", "{dir}"],
            console: false,
        };
        let args: Vec<String> = t
            .args
            .iter()
            .map(|a| a.replace("{dir}", "C:\\work"))
            .collect();
        assert_eq!(args, vec!["--cd".to_string(), "C:\\work".to_string()]);
    }
}
