import type { Session, CoreEvent } from "@harness/protocol";
import { checkForUpdate, detectInstallMethod, type InstallInfo, type SettingsStore } from "@harness/core";

/** The slice of ControlPlane the UpdateManager needs (ControlPlane satisfies it). */
export interface NotifyTarget {
  listSessions(): Session[];
  emit(e: CoreEvent): void;
}

export interface UpdateManagerDeps {
  cp: NotifyTarget;
  settings: SettingsStore;
  version: string;
  execPath: string;
  compiled: boolean;
  fetchImpl?: typeof fetch;
  dockerEnv?: boolean;
  home?: string;
  log?: (m: string) => void;
  makeTimer?: (fn: () => void, ms: number) => { stop(): void };
}

type Mode = "auto" | "notify" | "off";

export class UpdateManager {
  private timer?: { stop(): void };

  constructor(private deps: UpdateManagerDeps) {}

  mode(): Mode {
    const v = this.deps.settings.get("auto_update");
    return v === "notify" || v === "off" ? v : "auto";
  }

  start(): void {
    if (this.mode() === "off") return;
    void this.tick(); // initial check on boot
    const ms = Number(this.deps.settings.get("auto_update_check_interval_ms") ?? "21600000");
    const make =
      this.deps.makeTimer ??
      ((fn, every) => {
        const h = setInterval(fn, every);
        return { stop: () => clearInterval(h) };
      });
    this.timer = make(() => void this.tick(), ms);
  }

  stop(): void {
    this.timer?.stop();
    this.timer = undefined;
  }

  async tick(): Promise<void> {
    if (this.mode() === "off") return;
    const repo = this.deps.settings.get("auto_update_repo") ?? "alfin-efendy/harness-router";
    const res = await checkForUpdate({ currentVersion: this.deps.version, repo, fetchImpl: this.deps.fetchImpl });
    if (!res.updateAvailable || !res.latestVersion) return;
    const install = detectInstallMethod({
      execPath: this.deps.execPath,
      compiled: this.deps.compiled,
      home: this.deps.home,
      dockerEnv: this.deps.dockerEnv,
    });
    // Phase 2a: announce only. Phase 2b adds the self-apply branch for
    // mode === "auto" && install.selfApplicable.
    this.notify(res.latestVersion, install);
  }

  private notify(version: string, install: InstallInfo): void {
    if (this.deps.settings.get("last_notified_version") === version) return; // dedupe
    this.deps.settings.set("last_notified_version", version);
    const text = `⬆️ harness-router ${version} is available — ${upgradeHint(install)}`;
    this.deps.log?.(text);
    for (const s of this.deps.cp.listSessions()) {
      if (s.status !== "idle" && s.status !== "running") continue;
      this.deps.cp.emit({ kind: "notice", sessionPk: s.sessionPk, text });
    }
  }
}

function upgradeHint(install: InstallInfo): string {
  switch (install.method) {
    case "brew":
      return "run `brew upgrade harness-router` to update.";
    case "npm":
      return "run `npm i -g hrctl@latest` to update.";
    case "scoop":
      return "run `scoop update harness-router` to update.";
    case "installsh":
      return "run `curl -fsSL https://github.com/alfin-efendy/harness-router/raw/main/install.sh | sh` to update.";
    default:
      return "see the GitHub release to update.";
  }
}
