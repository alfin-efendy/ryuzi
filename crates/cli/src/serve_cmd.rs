//! `ryuzi serve` — run the HTTP surface over an embedded control plane so
//! external clients (or a remote `attach`) can drive and observe sessions.

use ryuzi_core::serve;
use ryuzi_core::ControlPlane;

use crate::dispatch::Deps;

const DEFAULT_PORT: u16 = 4096;

pub fn cmd_serve(args: &[String], deps: &mut Deps) -> u8 {
    let port = parse_port(args).unwrap_or(DEFAULT_PORT);
    let rt = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .expect("tokio runtime");
    rt.block_on(run_serve(port, deps))
}

/// Parse `--port N` from the args, if present.
fn parse_port(args: &[String]) -> Option<u16> {
    let mut it = args.iter();
    while let Some(a) = it.next() {
        if a == "--port" {
            return it.next().and_then(|v| v.parse().ok());
        }
        if let Some(v) = a.strip_prefix("--port=") {
            return v.parse().ok();
        }
    }
    None
}

async fn run_serve(port: u16, deps: &mut Deps) -> u8 {
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
    // The orch dispatcher runs here so `ryuzi orch submit` works headless.
    // (Its SQL claim transactions are safe if another dispatcher, e.g. the
    // daemon's, is also running.) `serve` does NOT host the cron scheduler:
    // post-Phase-1, the scheduler is daemon-only (`ryuzi __daemon` /
    // Cockpit's `--engine-daemon`, see `ryuzi_core::daemon::Daemon`) because
    // its last-fired read-check-write anchor is NOT cross-process safe — a
    // second host running it could double-fire jobs.
    ryuzi_core::orch::spawn_runner(cp.clone());
    let router_server = std::sync::Arc::new(ryuzi_core::llm_router::server::RouterServer::new(
        cp.store().clone(),
    ));
    let state = serve::ApiState {
        cp: cp.clone(),
        router_server,
        token: None,
    };
    let bound = match serve::serve(state, port).await {
        Ok(p) => p,
        Err(e) => {
            (deps.err)(&format!("✗ could not bind :{port}: {e}"));
            return 1;
        }
    };
    (deps.out)(&format!(
        "ryuzi serving on http://127.0.0.1:{bound} (GET /health, /sessions, /events; POST /sessions/:pk/prompt)"
    ));
    // Serve until interrupted.
    let _ = tokio::signal::ctrl_c().await;
    (deps.out)("shutting down");
    0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_port_forms() {
        assert_eq!(parse_port(&["--port".into(), "8080".into()]), Some(8080));
        assert_eq!(parse_port(&["--port=9000".into()]), Some(9000));
        assert_eq!(parse_port(&[]), None);
    }
}
