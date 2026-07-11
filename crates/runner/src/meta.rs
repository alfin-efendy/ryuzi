pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn help_text() -> String {
    [
        "ryuzi - headless engine daemon for Ryuzi Cockpit",
        "",
        "USAGE",
        "  ryuzi <command> [options]",
        "",
        "OPTIONS",
        "  -h, --help         show this help",
        "  -v, --version      print version",
        "",
        "COMMANDS",
        "  setup              first-run wizard: prompt for required settings",
        "  start              run the engine daemon in the foreground (Ctrl-C / SIGTERM to stop)",
        "  doctor             check your environment (git, settings)",
        "  config             read/write settings: ryuzi config <get|set|list> ...",
    ]
    .join("\n")
}
