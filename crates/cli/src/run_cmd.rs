use std::path::Path;

use ryuzi_core::domain::PermMode;
use ryuzi_core::settings::expand_home;
use ryuzi_core::{ControlPlane, CoreEvent};

use crate::dispatch::Deps;

const PERM_MODES: [&str; 4] = ["default", "acceptEdits", "bypassPermissions", "plan"];
const HARNESSES: [&str; 2] = ["claude-code", "native"];
const USAGE: &str =
    "usage: ryuzi run --dir <git-repo> --prompt <text> [--harness native|claude-code] [--model x] [--effort y] [--mode m]";

fn parse_mode(s: &str) -> Option<PermMode> {
    match s {
        "default" => Some(PermMode::Default),
        "acceptEdits" => Some(PermMode::AcceptEdits),
        "bypassPermissions" => Some(PermMode::BypassPermissions),
        "plan" => Some(PermMode::Plan),
        _ => None,
    }
}

/// A turn is over when the session row is no longer Running. Used only as a
/// fallback when the broadcast dropped the terminal event: at that point we
/// cannot distinguish success from error (errors are not persisted), so the
/// optimistic "✓ done" + exit 0 is the documented trade-off.
async fn turn_is_over(cp: &ControlPlane, session_pk: &str) -> bool {
    match cp.list_sessions(None).await {
        Ok(sessions) => sessions
            .iter()
            .find(|s| s.session_pk == session_pk)
            .map(|s| s.status != ryuzi_core::domain::SessionStatus::Running)
            .unwrap_or(true),
        Err(_) => false,
    }
}

pub fn cmd_run(args: &[String], deps: &mut Deps) -> u8 {
    let matches = clap::Command::new("run")
        .disable_help_flag(true)
        .arg(clap::Arg::new("dir").long("dir"))
        .arg(clap::Arg::new("prompt").long("prompt"))
        .arg(clap::Arg::new("model").long("model"))
        .arg(clap::Arg::new("effort").long("effort"))
        .arg(clap::Arg::new("mode").long("mode"))
        .arg(clap::Arg::new("harness").long("harness"))
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
    // `--harness` is optional: `None` means "use the project default / stored
    // value"; `Some` is an explicit choice honored for new AND existing projects.
    let harness = get("harness");
    if let Some(h) = &harness {
        if !HARNESSES.contains(&h.as_str()) {
            (deps.err)(&format!(
                "--harness must be one of: {}",
                HARNESSES.join(", ")
            ));
            return 1;
        }
    }

    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(run_session(
        &dir,
        &prompt,
        model,
        effort,
        mode,
        harness.as_deref(),
        deps,
    ))
}

#[allow(clippy::too_many_arguments)]
async fn run_session(
    dir: &str,
    prompt: &str,
    model: Option<String>,
    effort: Option<String>,
    mode: Option<PermMode>,
    harness: Option<&str>,
    deps: &mut Deps,
) -> u8 {
    let workdir = match std::fs::canonicalize(expand_home(dir)) {
        Ok(p) => p,
        Err(e) => {
            (deps.err)(&format!("✗ --dir {dir}: {e}"));
            return 1;
        }
    };

    let store = match crate::db::open_store(deps).await {
        Ok(s) => s,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };

    let registries = match (deps.build_registries)() {
        Ok(r) => r,
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
        // Existing project: model/effort/mode stay as stored, but an explicit
        // `--harness` that differs is honored (updated) — otherwise passing
        // `--harness native` on a project first connected as claude-code would
        // silently fail with "unknown harness 'claude-code'".
        Some(p) => match harness {
            Some(h) if h != p.harness => {
                match cp
                    .store()
                    .update_project(&p.project_id, p.model.clone(), p.perm_mode, h)
                    .await
                {
                    Ok(Some(updated)) => updated,
                    Ok(None) => p,
                    Err(e) => {
                        (deps.err)(&format!("✗ {e}"));
                        return 1;
                    }
                }
            }
            _ => p,
        },
        None => {
            let name = workdir
                .file_name()
                .map(|s| s.to_string_lossy().into_owned())
                .unwrap_or_else(|| workdir_str.clone());
            let p = match cp
                .connect_project_with_harness(
                    Path::new(&workdir),
                    &name,
                    harness.unwrap_or("claude-code"),
                )
                .await
            {
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

    let session = match cp
        .start_session(&project.project_id, prompt, "cli", &[])
        .await
    {
        Ok(s) => s,
        Err(e) => {
            (deps.err)(&format!("✗ {e}"));
            return 1;
        }
    };

    let mut failed = false;
    loop {
        // Bounded wait: if the terminal Result/Error is ever lost (broadcast
        // Lagged past capacity 1024), fall back to polling the session row so
        // a one-shot run can never hang forever.
        let recv = tokio::time::timeout(std::time::Duration::from_secs(2), rx.recv()).await;
        let event = match recv {
            Err(_elapsed) => {
                if turn_is_over(&cp, &session.session_pk).await {
                    (deps.out)("✓ done");
                    break;
                }
                continue;
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => {
                if turn_is_over(&cp, &session.session_pk).await {
                    (deps.out)("✓ done");
                    break;
                }
                continue;
            }
            Ok(Err(_closed)) => break,
            Ok(Ok(ev)) => ev,
        };
        match event {
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
        }
    }
    // Let `cp` drop with the runtime: AcpSession teardown kills the sidecar child.
    if failed {
        1
    } else {
        0
    }
}
