mod accent;
mod agents_cmd;
mod backdrop;
mod commands;
mod error;
mod events;
mod gateways_cmd;
mod providers_cmd;
mod scheduler_cmd;

use ryuzi_core::{AcpAdapterDescriptor, ClaudeCodeIntegration, ControlPlane, Registries, Store};
use tauri::Manager;
use tauri_specta::{collect_commands, collect_events, Builder};

/// The base name of the ACP adapter sidecar binary (no target-triple suffix,
/// no `.exe`).  Must match the `bundle.externalBin` entry in tauri.conf.json
/// and the filename produced by `apps/cockpit/scripts/build-acp-sidecar.ts`.
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
        let bin_name = if cfg!(windows) { "claude.exe" } else { "claude" };
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

/// Build the extension registries and install the `claude-agent-acp` harness
/// integration with the resolved adapter path.
///
/// `env_remove: ["CLAUDECODE"]` is required: the adapter checks for this
/// variable to detect a nested Claude Code session and refuses to start.
fn build_registries() -> Registries {
    let mut env = Vec::new();
    if let Some(cli) = resolve_claude_code_executable() {
        env.push(("CLAUDE_CODE_EXECUTABLE".to_string(), cli));
    }
    let descriptor = AcpAdapterDescriptor {
        command: resolve_acp_adapter_command(),
        args: vec![],
        env,
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
            commands::backdrop_capability,
            commands::get_setting,
            commands::set_setting,
            commands::update_project,
            agents_cmd::list_agents,
            agents_cmd::refresh_agents,
            agents_cmd::update_agent,
            agents_cmd::set_agent_tier,
            agents_cmd::set_default_agent,
            gateways_cmd::list_gateways,
            gateways_cmd::probe_gateways,
            gateways_cmd::add_gateway,
            gateways_cmd::remove_gateway,
            gateways_cmd::update_gateway,
            gateways_cmd::gateway_events,
            providers_cmd::list_providers,
            providers_cmd::add_provider,
            providers_cmd::remove_provider,
            providers_cmd::update_provider,
            providers_cmd::add_provider_account,
            providers_cmd::remove_provider_account,
            providers_cmd::set_active_account,
            providers_cmd::move_provider_account,
            scheduler_cmd::list_jobs,
            scheduler_cmd::create_job,
            scheduler_cmd::update_job,
            scheduler_cmd::toggle_job,
            scheduler_cmd::delete_job,
            scheduler_cmd::run_job_now,
            scheduler_cmd::parse_natural_schedule,
            accent::system_accent_color,
        ])
        .events(collect_events![events::CoreEventMsg, accent::AccentChangedMsg])
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
            // The scheduler loop fires enabled jobs for real (30s tick).
            ryuzi_core::scheduler::spawn_runner(cp.clone());
            // Make Arc<ControlPlane> available to all Tauri commands.
            app.manage(cp);
            // Apply the OS backdrop (mica/vibrancy) at runtime and record what
            // actually applied. Static windowEffects config is forbidden: Tauri
            // picks effects by platform family and swallows failures, which on
            // Win10 would yield a transparent window with no backdrop.
            let main_window = app
                .get_webview_window("main")
                .expect("main window exists");
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

    #[test]
    fn sdk_platform_package_maps_windows_x86_64() {
        assert_eq!(sdk_platform_package("windows", "x86_64"), "claude-agent-sdk-win32-x64");
    }

    #[test]
    fn sdk_platform_package_maps_macos_aarch64() {
        assert_eq!(sdk_platform_package("macos", "aarch64"), "claude-agent-sdk-darwin-arm64");
    }

    #[test]
    fn sdk_platform_package_maps_linux_x86_64() {
        assert_eq!(sdk_platform_package("linux", "x86_64"), "claude-agent-sdk-linux-x64");
    }
}
