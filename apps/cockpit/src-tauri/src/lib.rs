mod accent;
mod agent_cmd;
mod apps_cmd;
mod backdrop;
mod commands;
mod connections_cmd;
mod endpoint_cmd;
mod error;
mod events;
mod fsview_cmd;
mod gateways_cmd;
mod native_cmd;
mod open_cmd;
mod plugins_cmd;
mod scheduler_cmd;
mod session_io;
mod skills_cmd;
mod term;

use ryuzi_core::harness::native::native_plugin;
use ryuzi_core::{ControlPlane, Registries, Store};
use tauri::Manager;
use tauri_specta::{collect_commands, collect_events, Builder};

/// Build the extension registries: the in-process `native` harness (the
/// only runtime) plus the built-in discord gateway plugin.
fn build_registries() -> Registries {
    let mut registries = Registries::new();
    registries.add_plugin(native_plugin());
    registries.add_plugin(ryuzi_core::plugins::builtin::discord_plugin());
    ryuzi_core::plugins::install_builtins(&mut registries);
    ryuzi_core::plugins::load_skill_pack_plugins(&mut registries);
    registries
}

fn make_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            commands::list_projects,
            commands::list_sessions,
            commands::list_messages,
            commands::connect_project,
            commands::clone_project,
            commands::start_session,
            commands::continue_session,
            commands::stop_session,
            commands::end_session,
            commands::list_tool_policies,
            commands::delete_tool_policy,
            commands::resolve_approval,
            commands::read_file,
            commands::stage_attachment,
            commands::read_file_base64,
            commands::pick_directory,
            commands::pick_files,
            commands::backdrop_capability,
            commands::get_setting,
            commands::set_setting,
            commands::update_project,
            commands::list_branches,
            agent_cmd::get_agent_settings,
            agent_cmd::set_agent_settings,
            agent_cmd::list_selectable_models,
            gateways_cmd::list_gateways,
            gateways_cmd::probe_gateways,
            gateways_cmd::add_gateway,
            gateways_cmd::remove_gateway,
            gateways_cmd::update_gateway,
            gateways_cmd::gateway_events,
            scheduler_cmd::list_jobs,
            scheduler_cmd::create_job,
            scheduler_cmd::update_job,
            scheduler_cmd::toggle_job,
            scheduler_cmd::delete_job,
            scheduler_cmd::run_job_now,
            scheduler_cmd::parse_natural_schedule,
            apps_cmd::list_apps,
            apps_cmd::add_app,
            apps_cmd::remove_app,
            apps_cmd::probe_app,
            apps_cmd::update_app_scope,
            apps_cmd::set_app_tool_perm,
            apps_cmd::toggle_app_agent,
            fsview_cmd::list_dir,
            fsview_cmd::session_workdir,
            fsview_cmd::file_exists,
            fsview_cmd::worktree_dirty,
            fsview_cmd::git_diff,
            fsview_cmd::search_files,
            fsview_cmd::revert_file,
            open_cmd::list_open_targets,
            open_cmd::open_in,
            term::term_open,
            term::term_input,
            term::term_resize,
            term::term_close,
            term::term_close_session,
            accent::system_accent_color,
            endpoint_cmd::endpoint_status,
            endpoint_cmd::start_endpoint,
            endpoint_cmd::stop_endpoint,
            endpoint_cmd::set_endpoint_config,
            endpoint_cmd::list_endpoint_keys,
            endpoint_cmd::create_endpoint_key,
            endpoint_cmd::revoke_endpoint_key,
            endpoint_cmd::connection_usage,
            endpoint_cmd::endpoint_usage,
            connections_cmd::list_provider_catalog,
            connections_cmd::list_connections,
            connections_cmd::add_connection,
            connections_cmd::update_connection,
            connections_cmd::remove_connection,
            connections_cmd::move_connection,
            connections_cmd::test_connection,
            connections_cmd::test_connection_model,
            connections_cmd::refresh_provider_models,
            connections_cmd::list_model_statuses,
            connections_cmd::list_all_model_statuses,
            connections_cmd::connection_provider_quota,
            connections_cmd::reset_codex_credit,
            connections_cmd::list_model_routes,
            connections_cmd::save_model_route,
            connections_cmd::delete_model_route,
            connections_cmd::provider_account_route,
            connections_cmd::set_provider_account_route,
            connections_cmd::connect_oauth,
            connections_cmd::reconnect_oauth,
            connections_cmd::begin_oauth_manual,
            connections_cmd::complete_oauth_manual,
            connections_cmd::add_free_connection,
            native_cmd::native_agents,
            native_cmd::native_commands,
            native_cmd::session_todos,
            skills_cmd::list_skills,
            skills_cmd::install_skill,
            skills_cmd::remove_skill,
            skills_cmd::refresh_skill,
            plugins_cmd::list_plugins,
            plugins_cmd::plugin_detail,
            plugins_cmd::set_plugin_enabled,
            plugins_cmd::set_plugin_setting,
            plugins_cmd::uninstall_plugin,
            plugins_cmd::begin_plugin_oauth,
            plugins_cmd::complete_plugin_oauth,
            plugins_cmd::disconnect_plugin_oauth,
            plugins_cmd::plugin_models,
            plugins_cmd::begin_plugin_install,
            plugins_cmd::set_plugin_oauth_client_id,
            plugins_cmd::cancel_plugin_install,
            session_io::export_session,
            session_io::import_session,
            session_io::share_session,
            connections_cmd::start_kiro_device_flow,
            connections_cmd::await_kiro_device_flow,
            connections_cmd::import_kiro_token,
            connections_cmd::start_device_flow,
            connections_cmd::await_device_flow,
        ])
        .events(collect_events![
            events::CoreEventMsg,
            events::OauthAuthorizeUrlMsg,
            events::PluginOauthAuthorizeUrlMsg,
            events::PluginOauthCompletedMsg,
            accent::AccentChangedMsg,
            term::TermOutputMsg,
            term::TermExitMsg
        ])
}

/// Write `src/bindings.ts` for the current command/event surface. Used by the
/// dev-run export, the `export_bindings` test, and the `gen-bindings` bin
/// (which exists because the Windows lib-test harness crashes at startup —
/// tauri-apps/tauri#13419 — while bin artifacts get the app manifest linked).
/// The bin is feature-gated (`required-features = ["gen-bindings"]`) so
/// `tauri build` never bundles it; run it via `cargo gen-bindings` (alias in
/// the repo-root .cargo/config.toml).
pub fn export_bindings(out: &std::path::Path) {
    make_builder()
        .export(
            specta_typescript::Typescript::default()
                .bigint(specta_typescript::BigIntExportBehavior::Number),
            out,
        )
        .expect("export bindings");
}

pub fn run() {
    let builder = make_builder();

    #[cfg(debug_assertions)]
    {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
        export_bindings(&out);
    }

    tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .plugin(tauri_plugin_autostart::init(
            tauri_plugin_autostart::MacosLauncher::LaunchAgent,
            None,
        ))
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            // Build the engine inside the async runtime so Store::open (and any
            // harness setup) run within a Tokio context.
            let (cp, attachments_root) = tauri::async_runtime::block_on(async move {
                let store = Store::open(&ryuzi_core::paths::db_path())
                    .await
                    .expect("open ryuzi db");
                // One-time (idempotent) upgrade of any legacy plaintext
                // secrets to encrypted-at-rest; see
                // `llm_router::secrets::init_and_sweep`'s doc for the
                // atomicity/idempotency/degraded-state contract.
                ryuzi_core::llm_router::secrets::init_and_sweep(&store).await;
                let registries = build_registries();
                let cp = ControlPlane::new(store, registries).await;
                // Computed here (rather than a second `block_on`) because the
                // async runtime does not support nested `block_on` calls.
                let attachments_root = cp.attachments_root().await;
                (cp, attachments_root)
            });
            // Media previews: serve attachment files to the webview via the
            // asset protocol, scoped to the attachments root ONLY. The root
            // derives from the runtime-configurable `workdir_root` setting,
            // so the scope is extended here rather than in tauri.conf.json.
            let _ = std::fs::create_dir_all(&attachments_root);
            // Pasted files staged before a send belong to no session — clear
            // leftovers from previous runs.
            let _ = std::fs::remove_dir_all(attachments_root.join("staging"));
            if let Err(e) = app
                .asset_protocol_scope()
                .allow_directory(&attachments_root, true)
            {
                eprintln!("[ryuzi] asset protocol scope: {e}");
            }
            // Subscribe BEFORE manage() moves the Arc.
            let mut rx = cp.subscribe();
            // The scheduler loop fires enabled jobs for real (30s tick). Runs
            // on the tauri async runtime — setup() has no ambient tokio context.
            tauri::async_runtime::spawn(ryuzi_core::scheduler::run_loop(cp.clone()));
            // The orch dispatcher drives auto-decomposed task graphs (5s tick).
            tauri::async_runtime::spawn(ryuzi_core::orch::run_loop(cp.clone()));
            // Capture clones BEFORE app.manage(cp) moves the Arc away.
            let cp2 = cp.clone();
            // Make Arc<ControlPlane> available to all Tauri commands.
            app.manage(cp);
            // Local router endpoint server (Models → Endpoint).
            let router_srv = std::sync::Arc::new(
                ryuzi_core::llm_router::server::RouterServer::new(cp2.store().clone()),
            );
            app.manage(router_srv.clone());
            let cp3 = cp2.clone();
            tauri::async_runtime::spawn(async move {
                let auto = cp3
                    .store()
                    .get_setting("endpoint_autostart")
                    .await
                    .ok()
                    .flatten();
                if auto.as_deref() == Some("1") {
                    let port = endpoint_cmd::configured_port(&cp3).await;
                    if let Err(e) = router_srv.start(port).await {
                        eprintln!("[ryuzi] endpoint autostart failed: {e}");
                    }
                }
            });
            // UI terminal registry (session shells over portable-pty).
            app.manage(std::sync::Arc::new(term::UiTerms::default()));
            // Apply the OS backdrop (mica/vibrancy) at runtime and record what
            // actually applied. Static windowEffects config is forbidden: Tauri
            // picks effects by platform family and swallows failures, which on
            // Win10 would yield a transparent window with no backdrop.
            let main_window = app.get_webview_window("main").expect("main window exists");
            let cap = backdrop::apply_backdrop(&main_window);
            app.manage(backdrop::BackdropState(cap));
            accent::spawn_accent_watcher(app.handle());
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
    /// (On Windows prefer `cargo gen-bindings` — alias in .cargo/config.toml;
    /// the lib-test harness crashes at startup, tauri-apps/tauri#13419.)
    #[test]
    fn export_bindings_test() {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
        export_bindings(&out);
    }

    /// The native plugin (the only harness) must be present without any I/O.
    #[test]
    fn build_registries_registers_the_native_plugin() {
        let registries = build_registries();
        assert!(registries.plugins.get("native").is_some());
    }

    /// Regression test: Cockpit's composition root historically omitted
    /// `discord_plugin()`, so `list_plugins` omitted discord and
    /// `set_plugin_enabled("discord")` errored "unknown plugin: discord",
    /// diverging from the CLI and `ryuzi serve` (both of which register it —
    /// see `crates/cli/src/main.rs`'s `build_registries`).
    #[test]
    fn build_registries_registers_discord_plugin() {
        let registries = build_registries();
        assert!(
            registries.plugins.get("discord").is_some(),
            "discord plugin missing from Cockpit's composition root"
        );
    }
}
