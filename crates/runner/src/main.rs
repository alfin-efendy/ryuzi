use std::io::Write;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut deps = ryuzi_runner::dispatch::Deps {
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
        detect_git: ryuzi_runner::detect::detect_git,
    };
    ExitCode::from(ryuzi_runner::dispatch::run_cli(
        std::env::args().skip(1).collect(),
        &mut deps,
    ))
}
