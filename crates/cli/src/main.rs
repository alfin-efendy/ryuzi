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
    };
    ExitCode::from(ryuzi_cli::dispatch::run_cli(
        std::env::args().skip(1).collect(),
        &mut deps,
    ))
}
