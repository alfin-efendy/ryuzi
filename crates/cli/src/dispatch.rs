use std::path::PathBuf;

use crate::detect::Detected;
use crate::meta;

pub struct Deps {
    pub db_path: PathBuf,
    pub out: Box<dyn FnMut(&str)>,
    pub err: Box<dyn FnMut(&str)>,
    pub prompt: Box<dyn FnMut(&str) -> String>,
    pub detect_git: fn() -> Detected,
    pub detect_claude: fn() -> Detected,
    pub sidecar_status: Box<dyn Fn() -> ryuzi_core::sidecar::SidecarStatus>,
}

pub fn run_cli(args: Vec<String>, deps: &mut Deps) -> u8 {
    let cmd = args.first().map(String::as_str);
    match cmd {
        Some("-v") | Some("--version") => {
            (deps.out)(meta::version());
            0
        }
        Some("-h") | Some("--help") | Some("help") | None => {
            (deps.out)(&meta::help_text());
            0
        }
        Some("doctor") => crate::doctor::cmd_doctor(deps),
        Some(other) => {
            (deps.err)(&format!("unknown command: {other} - run `ryuzi --help`"));
            1
        }
    }
}
