#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

fn main() {
    if std::env::args().any(|a| a == "--engine-daemon") {
        std::process::exit(ryuzi_cockpit_lib::engine_daemon::run());
    }
    ryuzi_cockpit_lib::run();
}
