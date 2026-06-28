import pkg from "../../package.json";
import { brandName } from "./brand";
import { paint } from "./ui/theme";

export function version(): string {
  return (pkg as { version?: string }).version ?? "0.0.0";
}

export function helpText(): string {
  const h = (s: string) => paint(s, "dim", { bold: true }); // section heading
  const c = (s: string) => paint(s, "accent");              // command name
  return [
    `${paint(brandName, "signature", { bold: true })} — drive Claude Code from chat and terminal`,
    "",
    h("USAGE"),
    `  ${c("hr")}                 open the dashboard (first run launches setup)`,
    `  ${c("hr")} <command> [options]`,
    "",
    h("OPTIONS"),
    "  -h, --help         show this help",
    "  -v, --version      print version",
    "",
    h("COMMANDS"),
    `  ${c("doctor")}             check your environment (git, claude, settings)`,
    `  ${c("run")}                one-shot session:`,
    "                     hr run --dir <repo> --prompt <text> [--model x] [--effort y] [--mode m]",
  ].join("\n");
}
