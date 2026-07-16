mod accent;
mod agent_cmd;
mod apps_cmd;
mod audit_cmd;
mod automation_cmd;
mod backdrop;
mod commands;
mod connections_cmd;
mod delegation_cmd;
mod endpoint_cmd;
pub mod engine;
pub mod engine_daemon;
pub mod engine_manager;
mod error;
mod events;
mod fsview_cmd;
mod gateways_cmd;
mod learning_cmd;
mod native_cmd;
mod open_cmd;
mod plugins_cmd;
mod scheduler_cmd;
mod session_io;
mod skills_cmd;
mod term;

use tauri::Manager;
use tauri_specta::{collect_commands, collect_events, Builder};

fn make_builder() -> Builder<tauri::Wry> {
    Builder::<tauri::Wry>::new()
        .commands(collect_commands![
            commands::list_projects,
            commands::list_sessions,
            commands::list_agent_sessions,
            commands::list_messages,
            commands::connect_project,
            commands::clone_project,
            commands::start_session,
            commands::start_chat_session,
            commands::continue_session,
            commands::steer_session,
            commands::stop_session,
            commands::end_session,
            commands::list_tool_policies,
            commands::delete_tool_policy,
            commands::resolve_approval,
            commands::stage_attachment,
            commands::read_local_media,
            commands::fetch_attachment,
            commands::pick_directory,
            commands::pick_files,
            commands::backdrop_capability,
            commands::get_setting,
            commands::set_setting,
            commands::update_project,
            commands::update_project_perm_mode,
            commands::project_runtime_info,
            commands::update_project_runtime,
            commands::set_model_effort_preference,
            commands::session_runtime_info,
            commands::update_session_runtime,
            commands::update_session_perm_mode,
            commands::list_branches,
            delegation_cmd::get_child_runs,
            delegation_cmd::get_child_transcript,
            delegation_cmd::cancel_child_run,
            delegation_cmd::retry_child_run,
            agent_cmd::list_selectable_models,
            agent_cmd::list_agents,
            agent_cmd::get_agent,
            agent_cmd::create_agent,
            agent_cmd::update_agent,
            agent_cmd::duplicate_agent,
            agent_cmd::delete_agent,
            agent_cmd::set_default_agent,
            agent_cmd::get_subagent_model,
            agent_cmd::update_subagent_model,
            agent_cmd::get_agent_learning,
            agent_cmd::create_agent_concept,
            agent_cmd::update_agent_concept,
            agent_cmd::delete_agent_concept,
            agent_cmd::validate_agent_concept_raw,
            agent_cmd::replace_agent_concept_raw,
            agent_cmd::delete_invalid_agent_concept,
            agent_cmd::rollback_agent_learning,
            gateways_cmd::list_gateways,
            gateways_cmd::probe_gateways,
            gateways_cmd::add_gateway,
            gateways_cmd::add_runner,
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
            automation_cmd::list_automation_hooks,
            automation_cmd::automation_hook_detail,
            automation_cmd::create_automation_hook,
            automation_cmd::update_automation_hook,
            automation_cmd::toggle_automation_hook,
            automation_cmd::delete_automation_hook,
            automation_cmd::test_automation_hook,
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
            fsview_cmd::read_file,
            fsview_cmd::read_file_base64,
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
            connections_cmd::rename_connection,
            connections_cmd::set_connection_enabled,
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
            connections_cmd::list_model_route_target_capabilities,
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
            native_cmd::list_project_commands,
            native_cmd::read_project_command,
            native_cmd::create_project_command,
            native_cmd::update_project_command,
            native_cmd::delete_project_command,
            native_cmd::session_queue,
            native_cmd::enqueue_session_message,
            native_cmd::remove_session_message,
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
            plugins_cmd::begin_skill_install,
            plugins_cmd::confirm_skill_install,
            plugins_cmd::update_plugin,
            plugins_cmd::update_all_plugins,
            plugins_cmd::set_plugin_pin,
            plugins_cmd::plugin_doctor,
            plugins_cmd::plugins_restart_required,
            learning_cmd::search_sessions,
            learning_cmd::list_skill_usage,
            learning_cmd::set_skill_pinned,
            audit_cmd::list_audit,
            plugins_cmd::refresh_catalog,
            plugins_cmd::catalog_status,
            plugins_cmd::extension_status,
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

    let bindings = std::fs::read_to_string(out).expect("read exported bindings");
    let normalized = normalize_binding_whitespace(&bindings);
    if normalized != bindings {
        std::fs::write(out, normalized).expect("write normalized bindings");
    }
}

fn normalize_binding_whitespace(bindings: &str) -> String {
    let mut normalized = String::with_capacity(bindings.len());

    for line in bindings.split_inclusive('\n') {
        let (content, line_ending) = match line.strip_suffix('\n') {
            Some(line) => match line.strip_suffix('\r') {
                Some(line) => (line, "\r\n"),
                None => (line, "\n"),
            },
            None => (line, ""),
        };
        normalized.push_str(content.trim_end_matches([' ', '\t']));
        normalized.push_str(line_ending);
    }

    normalized
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
        .plugin(tauri_plugin_notification::init())
        .invoke_handler(builder.invoke_handler())
        .setup(move |app| {
            builder.mount_events(app);
            // Cockpit no longer embeds the engine: attach to a live engine
            // daemon or spawn one, then talk to it exclusively over
            // `EngineClient` (the daemon's HTTP control API). The attachments
            // root is derived engine-side (`workdir_root` setting) — fetch it
            // over the same RPC so the asset-protocol scope below still works.
            //
            // P3-3: Cockpit now talks to `EngineManager`'s runnerId->client
            // map instead of a single `Arc<EngineClient>`. `"local"` is
            // seeded first (and its attachments root fetched over it, same
            // as before); paired remote runners load afterwards, each
            // getting its own pinned `EngineClient` and its own SSE bridge
            // (`engine_manager::spawn_bridge`, fanned out one-per-runner —
            // see that module's docs for how the decrypted device token used
            // to build each remote client stays backend-only).
            let app_handle = app.handle().clone();
            let (manager, attachments_root) = tauri::async_runtime::block_on(async move {
                let (manager, local_client) =
                    crate::engine_manager::EngineManager::bootstrap_local()
                        .await
                        .expect("engine daemon unreachable");
                let root: String = local_client
                    .rpc("attachments_root", serde_json::json!({}))
                    .await
                    .expect("attachments root");
                manager.start_bridge("local".to_string(), local_client, &app_handle);
                if let Err(e) = manager.load_remotes(&app_handle).await {
                    eprintln!("[ryuzi] loading remote runners failed: {e}");
                }
                (manager, std::path::PathBuf::from(root))
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
            // P3-4: every engine-backed command now resolves its runner's
            // client through `EngineManager` directly (`runner_id: Option<
            // String>` param, defaulting to `"local"`) — the temporary P3-3
            // dual-manage of a bare `Arc<EngineClient>` is gone.
            app.manage(std::sync::Arc::new(manager));
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running ryuzi");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_trailing_horizontal_whitespace_without_changing_line_endings() {
        let source = "first \t\r\nsecond\t \nthird \t";

        assert_eq!(
            normalize_binding_whitespace(source),
            "first\r\nsecond\nthird"
        );
    }

    /// Generates `src/bindings.ts` without launching the Tauri GUI.
    /// Run via: `cargo test -p ryuzi-cockpit export_bindings -- --nocapture`
    /// (On Windows prefer `cargo gen-bindings` — alias in .cargo/config.toml;
    /// the lib-test harness crashes at startup, tauri-apps/tauri#13419.)
    #[test]
    fn export_bindings_test() {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
        export_bindings(&out);
    }

    #[test]
    fn exported_child_run_commands_match_the_plan6_contract() {
        let temp = tempfile::tempdir().unwrap();
        let out = temp.path().join("bindings.ts");
        export_bindings(&out);
        let bindings = std::fs::read_to_string(out).unwrap();

        for command in [
            "async getChildRuns(",
            "async getChildTranscript(",
            "async cancelChildRun(",
            "async retryChildRun(",
        ] {
            assert!(
                bindings.contains(command),
                "missing exported command {command}"
            );
        }
        let child_run_commands: Vec<_> = bindings
            .lines()
            .filter_map(|line| {
                let name = line.trim().strip_prefix("async ")?.split_once('(')?.0;
                (name.contains("Child") || name.contains("AgentRun")).then_some(name)
            })
            .collect();
        assert_eq!(
            child_run_commands,
            [
                "getChildRuns",
                "getChildTranscript",
                "cancelChildRun",
                "retryChildRun",
            ]
        );

        for draft_name in [
            "listAgentRuns",
            "getAgentRunTranscript",
            "stopAgentRun",
            "retryAgentRun",
        ] {
            assert!(
                !bindings.contains(draft_name),
                "obsolete draft command {draft_name} was exported"
            );
        }

        for contract in [
            "export type AgentRunRosterInfo = { rootRunId: string | null; runs: AgentRun[] }",
            "sourceToolCallId: string | null",
            "dispatchIndex: number | null",
            "{ kind: \"agentRunMessage\"; session_pk: string; run_id: string;",
        ] {
            assert!(
                bindings.contains(contract),
                "missing generated binding contract {contract}"
            );
        }
    }
}
