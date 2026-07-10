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
        build_registries: Box::new(|| {
            let mut registries = ryuzi_core::Registries::new();
            // The native runtime is the only harness — no external binary.
            registries.add_plugin(ryuzi_core::harness::native::native_plugin());
            // Discord is a built-in gateway; register it so `ryuzi plugins
            // enable/disable discord` recognizes it.
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
