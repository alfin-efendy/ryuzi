use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut deps = ryuzi_cli::dispatch::Deps {
        db_path: ryuzi_core::paths::db_path(),
        out: Box::new(|s| println!("{s}")),
        err: Box::new(|s| eprintln!("{s}")),
        prompt: Box::new(|q| {
            print!("{q}");
            let _ = std::io::stdout().flush();
            let mut line = String::new();
            let _ = std::io::stdin().read_line(&mut line);
            line
        }),
        detect_git: ryuzi_cli::detect::detect_git,
        detect_claude: ryuzi_cli::detect::detect_claude,
        sidecar_status: Box::new(|| ryuzi_cli::sidecar_host::manager().status()),
        build_registries: Box::new(|| {
            let mut registries = ryuzi_core::Registries::new();
            // The native runtime needs no external binary, so register it
            // unconditionally.
            registries.add_plugin(ryuzi_core::harness::native::native_plugin());
            // Claude Code needs the ACP sidecar. Resolving it may download the
            // bundled adapter; if that fails (offline, or a native-only setup),
            // skip it rather than failing the whole command — `--harness native`
            // still works.
            match ryuzi_cli::sidecar_host::manager().resolve() {
                Ok(resolved) => {
                    let descriptor = ryuzi_core::AcpAdapterDescriptor {
                        command: resolved.command,
                        args: resolved.args,
                        env: vec![],
                        // REQUIRED: the adapter refuses to start inside a nested
                        // Claude Code session.
                        env_remove: vec!["CLAUDECODE".to_string()],
                    };
                    registries.add_plugin(ryuzi_core::harness::acp::claude_code_plugin(descriptor));
                }
                Err(e) => {
                    eprintln!("note: claude-code harness unavailable ({e}); native runtime is still available");
                }
            }
            // Discord is a built-in gateway (its factory is a no-op unless the
            // `discord` feature is on, which this crate's Cargo.toml enables);
            // register it like `native`/`claude-code` so `ryuzi plugins
            // enable/disable discord` recognizes it — the store seeds
            // `enabled_gateways = "discord"` by default, so leaving this
            // unregistered made that setting refer to an "unknown plugin".
            registries.add_plugin(ryuzi_core::plugins::builtin::discord_plugin());
            ryuzi_core::plugins::install_builtins(&mut registries);
            ryuzi_core::plugins::load_skill_pack_plugins(&mut registries);
            Ok(registries)
        }),
    };
    ExitCode::from(ryuzi_cli::dispatch::run_cli(
        std::env::args().skip(1).collect(),
        &mut deps,
    ))
}
