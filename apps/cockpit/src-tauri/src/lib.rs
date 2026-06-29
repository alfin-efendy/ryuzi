mod commands;
mod error;
mod events;

use std::sync::Arc;
use harness_core::{ControlPlane, Store};
use tauri::Manager;
use tauri_specta::{collect_commands, collect_events, Builder};

fn resolve_hook_path(app: &tauri::AppHandle) -> String {
    // In dev, the hook binary is built into target/<profile>/harness-hook.
    // In a bundled app it ships alongside the main binary (see Task: packaging, R3).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let p = dir.join("harness-hook");
            if p.exists() {
                return p.to_string_lossy().into_owned();
            }
        }
    }
    // Dev fallback: fall through to PATH resolution in harness-core.
    let _ = app;
    "harness-hook".to_string()
}

fn make_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            commands::list_projects,
            commands::list_sessions,
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
            let handle = app.handle().clone();
            let hook_path = resolve_hook_path(&handle);
            // Build the engine AND enable approvals inside the async runtime: enable_approvals
            // calls tokio::runtime::Handle::current(), which panics ("no reactor running") unless
            // invoked from within a Tokio runtime context. block_on enters that context.
            let cp = tauri::async_runtime::block_on(async move {
                let store = Store::open(&harness_core::paths::db_path())
                    .await
                    .expect("open cockpit db");
                let cp = ControlPlane::new(store, Arc::new(harness_core::runtime::ProcessRunner)).await;
                // Enable the approval side-channel; errors are non-fatal (no hook binary in CI).
                cp.enable_approvals(hook_path).ok();
                cp
            });
            // Subscribe BEFORE manage() moves the Arc.
            let mut rx = cp.subscribe();
            // Make Arc<ControlPlane> available to all Tauri commands.
            app.manage(cp);
            // Bridge: forward every CoreEvent from the broadcast channel to the webview.
            let app_handle = app.handle().clone();
            tauri::async_runtime::spawn(async move {
                use tokio::sync::broadcast::error::RecvError;
                use tauri_specta::Event as _;
                loop {
                    match rx.recv().await {
                        Ok(ev) => {
                            let _ = events::CoreEventMsg { event: ev }.emit(&app_handle);
                        }
                        Err(RecvError::Lagged(n)) => {
                            eprintln!("[cockpit] CoreEvent bridge lagged, skipped {n} event(s)");
                            continue;
                        }
                        Err(RecvError::Closed) => break,
                    }
                }
            });
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running cockpit");
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Generates `src/bindings.ts` without launching the Tauri GUI.
    /// Run via: `cargo test -p cockpit export_bindings -- --nocapture`
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
