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
        "  serve              run the HTTP surface (GET /health,/sessions,/events; POST /sessions/:pk/prompt) [--port N]",
        "  orch               orchestrated task graphs: submit --project <id> <goal...> | list | cancel <id> | retry <id>",
        "  plugins            list/inspect/enable/disable plugins: ryuzi plugins <list|info|enable|disable> [id]",
    ]
    .join("\n")
}
