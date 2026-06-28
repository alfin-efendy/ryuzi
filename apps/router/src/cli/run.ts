import { openDb } from "../store/db";
import { SettingsStore } from "../config/store";
import { SETTING_DEFS } from "../config/schema";
import { missingRequiredSettings } from "../config/required";
import { catalog } from "../providers/catalog";
import type { detectClaude, detectGit } from "../harness/detect";
import type { Harness } from "../harness/types";
import { cmdRun } from "./run-command";
import { cmdServe } from "./serve-command";
import { launchUi } from "./ui/launch";
import { helpText, version } from "./meta";
import { runDaemon } from "./daemon-process";
import { paint } from "./ui/theme";

export interface IO {
  out(s: string): void;
  err(s: string): void;
  prompt(q: string): Promise<string>;
}

export interface CliDeps {
  io: IO;
  dbPath: string;
  detect: { claude: typeof detectClaude; git: typeof detectGit };
  harnessFactory?: () => Harness;
}

function redact(key: string, value: string): string {
  return SETTING_DEFS[key]?.secret ? "•".repeat(8) : value;
}

async function cmdConfig(args: string[], deps: CliDeps): Promise<number> {
  const settings = new SettingsStore(openDb(deps.dbPath));
  const sub = args[0];
  if (sub === "get") {
    const key = args[1];
    if (!key) {
      deps.io.err("usage: hr config get <key>");
      return 1;
    }
    deps.io.out(settings.get(key) ?? "");
    return 0;
  }
  if (sub === "set") {
    const [, key, value] = args;
    if (!key || value === undefined) {
      deps.io.err("usage: hr config set <key> <value>");
      return 1;
    }
    try {
      settings.set(key, value);
      deps.io.out(`set ${key}`);
      return 0;
    } catch (e) {
      deps.io.err((e as Error).message);
      return 1;
    }
  }
  if (sub === "list") {
    const persisted = settings.list();
    for (const key of Object.keys(SETTING_DEFS)) {
      const storedValue: string | undefined = persisted[key];
      if (storedValue !== undefined) {
        deps.io.out(`${key} = ${redact(key, storedValue)}`);
      } else {
        const defaultValue: string | undefined = SETTING_DEFS[key]?.default;
        if (defaultValue !== undefined) {
          deps.io.out(`${key} = ${redact(key, defaultValue)} (default)`);
        } else {
          deps.io.out(`${key} = (unset)`);
        }
      }
    }
    return 0;
  }
  deps.io.err("usage: hr config <get|set|list> ...");
  return 1;
}

async function cmdDoctor(deps: CliDeps): Promise<number> {
  const settings = new SettingsStore(openDb(deps.dbPath));
  const git = await deps.detect.git();
  const claude = await deps.detect.claude();
  const missing = missingRequiredSettings(settings, catalog);

  deps.io.out(`git:    ${git.found ? paint("OK", "ok") + " " + (git.version ?? "") : paint("NOT FOUND", "bad")}`);
  deps.io.out(`claude: ${claude.found ? paint("OK", "ok") + " " + (claude.version ?? "") : paint("NOT FOUND", "bad")}`);
  deps.io.out(`auth:   ${claude.found ? "unknown (relies on host login)" : "n/a"}`);
  deps.io.out(missing.length ? `settings: ${paint("missing " + missing.join(", "), "warn")}` : `settings: ${paint("OK", "ok")}`);

  const ok = git.found && claude.found && missing.length === 0;
  deps.io.out(ok ? `doctor: ${paint("PASS", "ok", { bold: true })}` : `doctor: ${paint("FAIL", "bad")}`);
  return ok ? 0 : 1;
}

export async function runCli(args: string[], deps: CliDeps): Promise<number> {
  const [cmd, ...rest] = args;
  switch (cmd) {
    case "doctor":
      return cmdDoctor(deps);
    case "run":
      return cmdRun(rest, deps);
    case "config": // hidden: kept for headless automation
      return cmdConfig(rest, deps);
    case "-v":
    case "--version":
      deps.io.out(version());
      return 0;
    case "-h":
    case "--help":
    case "help":
      deps.io.out(helpText());
      return 0;
    case "serve":
      return cmdServe(rest, deps);
    case "__daemon":
      await runDaemon({ dbPath: deps.dbPath });
      return 0;
    case undefined:
      return launchUi(deps);
    default:
      deps.io.err(`unknown command: ${cmd} — run \`hr --help\``);
      return 1;
  }
}
