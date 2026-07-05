mod accent;
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
mod plugins_cmd;
mod registry_cmd;
mod runtimes_cmd;
mod scheduler_cmd;
mod session_io;
mod term;

use ryuzi_core::harness::acp::claude_code_plugin_with_resolver;
use ryuzi_core::harness::native::native_plugin;
use ryuzi_core::{AcpAdapterDescriptor, ControlPlane, Registries, Store};
use tauri::Manager;
use tauri_specta::{collect_commands, collect_events, Builder};

/// The base name of the ACP adapter sidecar binary (no target-triple suffix,
/// no `.exe`). Used only as the dev/PATH fallback name when the shared
/// resolver (`ryuzi_core::sidecar::host_manager`) fails to resolve a cached
/// or downloaded artifact — see `resolve_acp_adapter` below.
///
/// Package: @agentclientprotocol/claude-agent-acp
/// NOTE: the adapter refuses to start inside a nested Claude Code session, so
/// we unconditionally remove the `CLAUDECODE` env-var before spawning it.
/// Authentication (claude login) is out-of-band — the host machine's `claude`
/// session is reused; no credentials are managed here.
const ADAPTER_BIN: &str = "claude-agent-acp";

/// Resolve the ACP adapter via the shared tiered resolver (Spec 4 §4):
/// RYUZI_ACP_PATH override → cached artifact → download (bun bundle if a
/// host bun exists, else the standalone binary). First resolve needs network
/// or Bun — the same contract as the CLI. Runs lazily on the first
/// `claude-code` session start (never at app launch — see
/// [`build_registries`]). On resolver failure we fall back to the bare
/// adapter name (dev PATH behavior): the session start then fails with a
/// clear spawn error instead.
fn resolve_acp_adapter() -> (String, Vec<String>) {
    match ryuzi_core::sidecar::host_manager().resolve() {
        Ok(r) => (r.command, r.args),
        Err(e) => {
            eprintln!("[ryuzi] sidecar resolve failed: {e:#}; falling back to PATH lookup of {ADAPTER_BIN}");
            (ADAPTER_BIN.to_string(), vec![])
        }
    }
}

/// Map Rust's `std::env::consts::{OS, ARCH}` to the npm platform-package
/// naming used by `@anthropic-ai/claude-agent-sdk`'s optional-dependency
/// binaries (e.g. `claude-agent-sdk-win32-x64`). Pure and testable in
/// isolation from the filesystem lookups in
/// [`resolve_claude_code_executable`].
fn sdk_platform_package(os: &str, arch: &str) -> String {
    let os = match os {
        "windows" => "win32",
        "macos" => "darwin",
        other => other,
    };
    let arch = match arch {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        other => other,
    };
    format!("claude-agent-sdk-{os}-{arch}")
}

/// Resolve a Claude Code CLI for the ACP adapter's `CLAUDE_CODE_EXECUTABLE`
/// override. The bun-compiled adapter cannot resolve
/// `@anthropic-ai/claude-agent-sdk` from its virtual filesystem (bunfs), so
/// the engine locates the CLI on its behalf:
///   1. Respect an operator-provided CLAUDE_CODE_EXECUTABLE (inherited env).
///   2. A bundled `claude-code[.exe]` next to the app executable (reserved
///      for future packaged builds; nothing bundles it yet).
///   3. Dev builds only: the SDK platform package inside the sidecar
///      isolated build dir produced by scripts/build-acp-sidecar.ts.
///
/// Returns None when nothing is found — the adapter then falls back to its
/// own resolution, which works when it runs un-compiled under bun/node.
fn resolve_claude_code_executable() -> Option<String> {
    // 1. Operator-provided override — the child inherits it either way, so
    // don't shadow it with our own resolution.
    if std::env::var_os("CLAUDE_CODE_EXECUTABLE").is_some() {
        return None;
    }

    // 2. Bundled `claude-code[.exe]` next to the app executable.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            #[cfg(windows)]
            let candidate = dir.join("claude-code.exe");
            #[cfg(not(windows))]
            let candidate = dir.join("claude-code");

            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }

    // 3. Dev builds only: the SDK platform package inside the isolated
    // sidecar build dir (see scripts/build-acp-sidecar.ts).
    #[cfg(debug_assertions)]
    {
        let bin_name = if cfg!(windows) {
            "claude.exe"
        } else {
            "claude"
        };
        let scope_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join(".sidecar-build")
            .join("node_modules")
            .join("@anthropic-ai");

        let os = std::env::consts::OS;
        let arch = std::env::consts::ARCH;
        // On Linux, both glibc and musl platform packages may be installed;
        // prefer glibc, mirroring the adapter's own resolution order.
        let candidates = if os == "linux" {
            vec![
                sdk_platform_package(os, arch),
                format!("{}-musl", sdk_platform_package(os, arch)),
            ]
        } else {
            vec![sdk_platform_package(os, arch)]
        };

        for pkg in candidates {
            let candidate = scope_dir.join(&pkg).join(bin_name);
            if candidate.exists() {
                return Some(candidate.to_string_lossy().into_owned());
            }
        }
    }

    None
}

/// Build the extension registries: the in-process `native` harness (needs no
/// external binary) plus the `claude-agent-acp` harness with a LAZY adapter
/// resolver.
///
/// Registration performs no sidecar I/O: the resolver (which may download the
/// adapter on first run) only executes when a `claude-code` session actually
/// starts. App launch therefore never blocks on the resolve, and a setup that
/// only runs the `native` harness never touches the resolver at all.
///
/// `env_remove: ["CLAUDECODE"]` is required: the adapter checks for this
/// variable to detect a nested Claude Code session and refuses to start.
fn build_registries() -> Registries {
    let mut registries = Registries::new();
    // The native runtime needs no external binary — register it unconditionally
    // so projects with `harness = "native"` work in the desktop app.
    registries.add_plugin(native_plugin());

    registries.add_plugin(claude_code_plugin_with_resolver(|| {
        let mut env = Vec::new();
        if let Some(cli) = resolve_claude_code_executable() {
            env.push(("CLAUDE_CODE_EXECUTABLE".to_string(), cli));
        }
        let (command, args) = resolve_acp_adapter();
        Ok(AcpAdapterDescriptor {
            command,
            args,
            env,
            // REQUIRED: strip CLAUDECODE so the adapter doesn't think it's
            // running inside a Claude Code session and refuses to start.
            env_remove: vec!["CLAUDECODE".to_string()],
        })
    }));

    // Discord is a built-in gateway (its factory is a no-op unless the
    // `discord` feature is on); register it like `native`/`claude-code` so
    // Cockpit's `list_plugins`/`set_plugin_enabled` recognize it — the store
    // seeds `enabled_gateways = "discord"` by default, so leaving this
    // unregistered made Cockpit diverge from the CLI/`serve`, which both
    // already register it (see `crates/cli/src/main.rs`'s `build_registries`).
    registries.add_plugin(ryuzi_core::plugins::builtin::discord_plugin());

    ryuzi_core::plugins::install_builtins(&mut registries);
    ryuzi_core::plugins::load_user_plugins(&mut registries);
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
            commands::backdrop_capability,
            commands::get_setting,
            commands::set_setting,
            commands::update_project,
            runtimes_cmd::list_runtimes,
            runtimes_cmd::refresh_runtimes,
            runtimes_cmd::update_runtime_config,
            runtimes_cmd::update_runtime,
            runtimes_cmd::set_runtime_tier,
            runtimes_cmd::set_default_runtime,
            runtimes_cmd::runtime_config_status,
            runtimes_cmd::apply_runtime_config,
            runtimes_cmd::reset_runtime_config,
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
            registry_cmd::registry_search,
            fsview_cmd::list_dir,
            fsview_cmd::session_workdir,
            fsview_cmd::worktree_dirty,
            fsview_cmd::git_diff,
            fsview_cmd::search_files,
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
            plugins_cmd::list_plugins,
            plugins_cmd::plugin_detail,
            plugins_cmd::set_plugin_enabled,
            plugins_cmd::set_plugin_setting,
            plugins_cmd::plugin_models,
            session_io::export_session,
            session_io::import_session,
            session_io::share_session,
            connections_cmd::start_kiro_device_flow,
            connections_cmd::await_kiro_device_flow,
            connections_cmd::import_kiro_token,
        ])
        .events(collect_events![
            events::CoreEventMsg,
            events::OauthAuthorizeUrlMsg,
            accent::AccentChangedMsg,
            term::TermOutputMsg,
            term::TermExitMsg
        ])
}

/// Write `src/bindings.ts` for the current command/event surface. Used by the
/// dev-run export, the `export_bindings` test, and the `gen-bindings` bin
/// (which exists because the Windows lib-test harness crashes at startup —
/// tauri-apps/tauri#13419 — while bin artifacts get the app manifest linked).
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
            let cp = tauri::async_runtime::block_on(async move {
                let store = Store::open(&ryuzi_core::paths::db_path())
                    .await
                    .expect("open ryuzi db");
                // One-time (idempotent) upgrade of any legacy plaintext
                // secrets to encrypted-at-rest; see
                // `llm_router::secrets::init_and_sweep`'s doc for the
                // atomicity/idempotency/degraded-state contract.
                ryuzi_core::llm_router::secrets::init_and_sweep(&store).await;
                let registries = build_registries();
                ControlPlane::new(store, registries).await
            });
            // Subscribe BEFORE manage() moves the Arc.
            let mut rx = cp.subscribe();
            // The scheduler loop fires enabled jobs for real (30s tick). Runs
            // on the tauri async runtime — setup() has no ambient tokio context.
            tauri::async_runtime::spawn(ryuzi_core::scheduler::run_loop(cp.clone()));
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
    /// (On Windows prefer `cargo run -p ryuzi-cockpit --bin gen-bindings` —
    /// the lib-test harness crashes at startup, tauri-apps/tauri#13419.)
    #[test]
    fn export_bindings_test() {
        let out = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../src/bindings.ts");
        export_bindings(&out);
    }

    /// Registration must be hermetic: both harnesses appear in the registry
    /// without any sidecar resolve (this test would touch the network under
    /// the old eager resolution — laziness is what makes it runnable at all).
    #[test]
    fn build_registries_registers_both_harnesses_without_sidecar_io() {
        let registries = build_registries();
        let names = registries.harness.names();
        assert!(names.iter().any(|n| n == "native"), "got: {names:?}");
        assert!(names.iter().any(|n| n == "claude-code"), "got: {names:?}");
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

    #[test]
    fn sdk_platform_package_maps_windows_x86_64() {
        assert_eq!(
            sdk_platform_package("windows", "x86_64"),
            "claude-agent-sdk-win32-x64"
        );
    }

    #[test]
    fn sdk_platform_package_maps_macos_aarch64() {
        assert_eq!(
            sdk_platform_package("macos", "aarch64"),
            "claude-agent-sdk-darwin-arm64"
        );
    }

    #[test]
    fn sdk_platform_package_maps_linux_x86_64() {
        assert_eq!(
            sdk_platform_package("linux", "x86_64"),
            "claude-agent-sdk-linux-x64"
        );
    }
}
