use std::io::IsTerminal;
use std::path::PathBuf;

use crate::detect::Detected;
use crate::meta;

pub struct Deps {
    pub db_path: PathBuf,
    pub out: Box<dyn FnMut(&str)>,
    pub err: Box<dyn FnMut(&str)>,
    pub prompt: Box<dyn FnMut(&str) -> String>,
    pub detect_git: fn() -> Detected,
    pub build_registries: Box<dyn Fn() -> anyhow::Result<ryuzi_core::Registries>>,
}

pub fn run_cli(args: Vec<String>, deps: &mut Deps) -> u8 {
    let cmd = args.first().map(String::as_str);
    match cmd {
        Some("-v") | Some("--version") => {
            (deps.out)(meta::version());
            0
        }
        Some("-h") | Some("--help") | Some("help") => {
            (deps.out)(&meta::help_text());
            0
        }
        // TTY gate: a bare `ryuzi` on a real terminal launches the TUI
        // (wizard first-run, else dashboard); piped/non-interactive stdout
        // (scripts, CI, the `cli.rs` no-args test which runs through
        // `assert_cmd`'s captured pipe) keeps printing help and exits 0, so
        // script-safe automation is unaffected.
        None => {
            if std::io::stdout().is_terminal() {
                crate::tui::launch_ui(deps)
            } else {
                (deps.out)(&meta::help_text());
                0
            }
        }
        Some("doctor") => crate::doctor::cmd_doctor(deps),
        Some("run") => crate::run_cmd::cmd_run(&args[1..], deps),
        Some("serve") => crate::serve_cmd::cmd_serve(&args[1..], deps),
        Some("orch") => crate::orch_cmd::cmd_orch(&args[1..], deps),
        Some("plugins") => crate::plugins_cmd::cmd_plugins(&args[1..], deps),
        Some("config") => crate::config_cmd::cmd_config(&args[1..], deps), // hidden: kept for headless automation
        Some("__daemon") => crate::daemon_cmd::cmd_daemon(&args[1..], deps), // hidden: spawned by the dashboard daemon toggle
        Some(other) => {
            (deps.err)(&format!("unknown command: {other} - run `ryuzi --help`"));
            1
        }
    }
}
