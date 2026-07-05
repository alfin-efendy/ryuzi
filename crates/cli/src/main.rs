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
            registries.install(&ryuzi_core::harness::native::NativeIntegration::new());
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
                    registries.install(&ryuzi_core::ClaudeCodeIntegration::new(descriptor));
                }
                Err(e) => {
                    eprintln!("note: claude-code harness unavailable ({e}); native runtime is still available");
                }
            }
            Ok(registries)
        }),
    };
    ExitCode::from(ryuzi_cli::dispatch::run_cli(
        std::env::args().skip(1).collect(),
        &mut deps,
    ))
}
