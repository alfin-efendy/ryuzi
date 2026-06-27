import pkg from "../../package.json";

export function version(): string {
  return (pkg as { version?: string }).version ?? "0.0.0";
}

export function helpText(): string {
  return [
    "hr — drive Claude Code from chat and terminal",
    "",
    "USAGE",
    "  hr                 open the dashboard (first run launches setup)",
    "  hr <command> [options]",
    "",
    "OPTIONS",
    "  -h, --help         show this help",
    "  -v, --version      print version",
    "",
    "COMMANDS",
    "  doctor             check your environment (git, claude, settings)",
    "  run                one-shot session:",
    "                     hr run --dir <repo> --prompt <text> [--model x] [--effort y] [--mode m]",
  ].join("\n");
}
