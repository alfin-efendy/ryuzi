pub fn version() -> &'static str {
    env!("CARGO_PKG_VERSION")
}

pub fn help_text() -> String {
    [
        "ryuzi - drive Claude Code from chat and terminal",
        "",
        "USAGE",
        "  ryuzi                 open the dashboard (first run launches setup)",
        "  ryuzi <command> [options]",
        "",
        "OPTIONS",
        "  -h, --help         show this help",
        "  -v, --version      print version",
        "",
        "COMMANDS",
        "  doctor             check your environment (git, claude, settings)",
        "  run                one-shot session:",
        "                     ryuzi run --dir <repo> --prompt <text> [--model x] [--effort y] [--mode m]",
    ]
    .join("\n")
}
