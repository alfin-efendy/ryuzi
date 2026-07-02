use std::path::{Path, PathBuf};

use ryuzi_core::domain::PermMode;
use ryuzi_core::{ControlPlane, CoreEvent, Store};

use crate::dispatch::Deps;

const PERM_MODES: [&str; 4] = ["default", "acceptEdits", "bypassPermissions", "plan"];
const USAGE: &str =
    "usage: ryuzi run --dir <git-repo> --prompt <text> [--model x] [--effort y] [--mode m]";

fn parse_mode(s: &str) -> Option<PermMode> {
    match s {
        "default" => Some(PermMode::Default),
        "acceptEdits" => Some(PermMode::AcceptEdits),
        "bypassPermissions" => Some(PermMode::BypassPermissions),
        "plan" => Some(PermMode::Plan),
        _ => None,
    }
}

fn expand_home(dir: &str) -> PathBuf {
    if let Some(rest) = dir.strip_prefix("~") {
        if let Some(home) = std::env::var_os("HOME") {
            return PathBuf::from(home).join(rest.trim_start_matches('/'));
        }
    }
    PathBuf::from(dir)
}

pub fn cmd_run(args: &[String], deps: &mut Deps) -> u8 {
    let matches = clap::Command::new("run")
        .disable_help_flag(true)
        .arg(clap::Arg::new("dir").long("dir"))
        .arg(clap::Arg::new("prompt").long("prompt"))
        .arg(clap::Arg::new("model").long("model"))
        .arg(clap::Arg::new("effort").long("effort"))
        .arg(clap::Arg::new("mode").long("mode"))
        .try_get_matches_from(std::iter::once("run".to_string()).chain(args.iter().cloned()));
    let matches = match matches {
        Ok(m) => m,
        Err(e) => {
            (deps.err)(&e.to_string());
            return 1;
        }
    };
    let get = |k: &str| matches.get_one::<String>(k).cloned();
    let (Some(dir), Some(prompt)) = (get("dir"), get("prompt")) else {
        (deps.err)(USAGE);
        return 1;
    };
    let mode = match get("mode") {
        None => None,
        Some(m) => match parse_mode(&m) {
            Some(p) => Some(p),
            None => {
                (deps.err)(&format!("--mode must be one of: {}", PERM_MODES.join(", ")));
                return 1;
            }
        },
    };
    let (model, effort) = (get("model"), get("effort"));

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(run_session(&dir, &prompt, model, effort, mode, deps))
}

async fn run_session(
    dir: &str,
    prompt: &str,
    model: Option<String>,
    effort: Option<String>,
    mode: Option<PermMode>,
    deps: &mut Deps,
) -> u8 {
    let workdir = match std::fs::canonicalize(expand_home(dir)) {
        Ok(p) => p,
        Err(e) => {
            (deps.err)(&format!("✗ --dir {dir}: {e}"));
            return 1;
        }
    };

    // Spec 4 §6 clean break: move a TS-schema db aside before Store::open.
    match ryuzi_core::store::quarantine_legacy_db(&deps.db_path) {
        Ok(Some(bak)) => (deps.err)(&format!(
            "note: existing database used the retired schema; moved to {}",
            bak.display()
        )),
        Ok(None) => {}
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    }

    let registries = match (deps.build_registries)() {
        Ok(r) => r,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    let store = match Store::open(&deps.db_path).await {
        Ok(s) => s,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    let cp = ControlPlane::new(store, registries).await;
    let mut rx = cp.subscribe(); // BEFORE start_session — broadcast drops events for late subscribers

    let workdir_str = workdir.to_string_lossy().into_owned();
    let existing = match cp.list_projects().await {
        Ok(ps) => ps.into_iter().find(|p| p.workdir == workdir_str),
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };
    let project = match existing {
        Some(p) => p, // TS parity: flags on an existing project row are silently ignored
        None => {
            let name = workdir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| workdir_str.clone());
            let p = match cp.connect_project(Path::new(&workdir), &name).await {
                Ok(p) => p,
                Err(e) => {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            };
            if model.is_some() || effort.is_some() || mode.is_some() {
                if let Err(e) = cp
                    .set_project_prefs(&p.project_id, model.as_deref(), effort.as_deref(), mode)
                    .await
                {
                    (deps.err)(&format!("✗ {e}"));
                    return 1;
                }
            }
            p
        }
    };

    let session = match cp.start_session(&project.project_id, prompt).await {
        Ok(s) => s,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };

    let mut failed = false;
    loop {
        match rx.recv().await {
            Ok(ev) => match ev {
                CoreEvent::Message {
                    session_pk,
                    role,
                    block_type,
                    payload,
                    ..
                } if session_pk == session.session_pk => match block_type.as_str() {
                    "status" => {
                        if let Some(s) = payload.get("summary").and_then(|v| v.as_str()) {
                            (deps.out)(&format!("· {s}"));
                        }
                    }
                    "text" if role == "assistant" => {
                        if let Some(t) = payload.get("text").and_then(|v| v.as_str()) {
                            (deps.out)(t);
                        }
                    }
                    _ => {} // thought/tool_call not rendered in 4A
                },
                CoreEvent::ApprovalRequested {
                    session_pk,
                    request_id,
                    tool,
                    summary,
                } if session_pk == session.session_pk => {
                    let answer = (deps.prompt)(&format!("approve {tool}? {summary} [y/N] "));
                    cp.resolve_approval(&request_id, answer.trim().eq_ignore_ascii_case("y"));
                }
                CoreEvent::Result { session_pk } if session_pk == session.session_pk => {
                    (deps.out)("✓ done");
                    break;
                }
                CoreEvent::Error {
                    session_pk,
                    message,
                } if session_pk == session.session_pk => {
                    failed = true;
                    (deps.err)(&format!("✗ {message}"));
                    break;
                }
                _ => {}
            },
            Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
            Err(_) => break,
        }
    }
    // Let `cp` drop with the runtime: AcpSession teardown kills the sidecar child.
    if failed {
        1
    } else {
        0
    }
}
