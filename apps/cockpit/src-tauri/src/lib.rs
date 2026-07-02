mod commands;
mod error;
mod events;

use ryuzi_core::{AcpAdapterDescriptor, ClaudeCodeIntegration, ControlPlane, Registries, Store};
use tauri::Manager;
use tauri_specta::{collect_commands, collect_events, Builder};

/// The base name of the ACP adapter sidecar binary (no target-triple suffix,
/// no `.exe`).  Must match the `bundle.externalBin` entry in tauri.conf.json
/// and the filename produced by `scripts/build-acp-sidecar.ts`.
///
/// Package: @agentclientprotocol/claude-agent-acp
/// NOTE: the adapter refuses to start inside a nested Claude Code session, so
/// we unconditionally remove the `CLAUDECODE` env-var before spawning it.
/// Authentication (claude login) is out-of-band — the host machine's `claude`
/// session is reused; no credentials are managed here.
const ADAPTER_BIN: &str = "claude-agent-acp";

/// Resolve the ACP adapter command, preferring the Tauri-bundled sidecar.
///
/// Resolution order:
///   1. Bundled sidecar: `<exe_dir>/claude-agent-acp[.exe]` — present in
///      production (tauri build) and after running build-acp-sidecar.ts.
///   2. PATH fallback — for `cargo build` / `tauri dev` when the sidecar
///      binary has not been compiled yet.  A missing PATH entry produces a
///      clear runtime error on first session start, not at launch.
fn resolve_acp_adapter_command() -> String {
    // Tauri places sidecars next to the main executable at runtime.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            // Check platform-specific name: <dir>/claude-agent-acp[.exe]
            #[cfg(windows)]
            let candidate = dir.join(format!("{}.exe", ADAPTER_BIN));
            #[cfg(not(windows))]
            let candidate = dir.join(ADAPTER_BIN);

            if candidate.exists() {
                return candidate.to_string_lossy().into_owned();
            }
        }
    }
    // Dev / PATH fallback — allows `cargo build` + `tauri dev` to compile and
    // launch without the sidecar binary present.  Starting a session without
    // the sidecar installed will fail with a clear spawn error at runtime.
    ADAPTER_BIN.to_string()
}

/// Build the extension registries and install the `claude-agent-acp` harness
/// integration with the resolved adapter path.
///
/// `env_remove: ["CLAUDECODE"]` is required: the adapter checks for this
/// variable to detect a nested Claude Code session and refuses to start.
fn build_registries() -> Registries {
    let descriptor = AcpAdapterDescriptor {
        command: resolve_acp_adapter_command(),
        args: vec![],
        env: vec![],
        // REQUIRED: strip CLAUDECODE so the adapter doesn't think it's running
        // inside a Claude Code session and refuses to start.
        env_remove: vec!["CLAUDECODE".to_string()],
    };
    let mut registries = Registries::new();
    registries.install(&ClaudeCodeIntegration::new(descriptor));
    registries
}

fn make_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            commands::list_projects,
            commands::list_sessions,
            commands::list_messages,
            commands::connect_project,
            commands::start_session,
            commands::continue_session,
            commands::stop_session,
            commands::end_session,
            commands::resolve_approval,
            commands::read_file,
            commands::pick_directory,
        ])
        .events(collect_events![events::CoreEventMsg])
}

pub fn run() {
    let builder = make_builder();

    #[cfg(debug_assertions)]
    {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
        builder
            .export(
                specta_typescript::Typescript::default()
                    .bigint(specta_typescript::BigIntExportBehavior::Number),
                &out,
            )
            .expect("export bindings");
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            // Build the engine inside the async runtime so Store::open (and any
            // harness setup) run within a Tokio context.
            let cp = tauri::async_runtime::block_on(async move {
                let store = Store::open(&ryuzi_core::paths::db_path())
                    .await
                    .expect("open ryuzi db");
                let registries = build_registries();
                ControlPlane::new(store, registries).await
            });
            // Subscribe BEFORE manage() moves the Arc.
            let mut rx = cp.subscribe();
            // Make Arc<ControlPlane> available to all Tauri commands.
            app.manage(cp);
            // Bridge: forward every CoreEvent from the broadcast channel to the webview.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tauri_specta::Event as _;
                use tokio::sync::broadcast::error::RecvError;
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            let _ = events::CoreEventMsg { event: ev }.emit(&app_handle);
                        }
                        Err(RecvError::Lagged(n)) => {
                            eprintln!("[ryuzi] CoreEvent bridge lagged, skipped {n} event(s)");
                            continue;
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running ryuzi");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generates `src/bindings.ts` without launching the Tauri GUI.
    /// Run via: `cargo test -p ryuzi-cockpit export_bindings -- --nocapture`
    #[test]
    fn export_bindings() {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
        make_builder()
            .export(
                specta_typescript::Typescript::default()
                    .bigint(specta_typescript::BigIntExportBehavior::Number),
                &out,
            )
            .expect("export bindings");
    }
}
