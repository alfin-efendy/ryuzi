import { EventEmitter } from "node:events";
import { existsSync, readFileSync, openSync, closeSync } from "node:fs";
import { dirname, join } from "node:path";
import type { Database } from "bun:sqlite";
import {
  openDb,
  SettingsStore,
  SETTING_DEFS,
  GLOBAL_FIELDS,
  allFields,
  SessionsStore,
  catalog as defaultCatalog,
  missingRequiredSettings,
  isConfigured as isConfiguredFn,
  requiredMissingFields,
  csv,
} from "@harness/core";
import type { detectClaude, detectGit, ToolInfo, ProviderCatalog, GatewayDescriptor, RuntimeDescriptor, ConfigField } from "@harness/core";
import { readStatus, writeStatus, clearStatus, isAlive, deriveState, type DaemonState } from "../daemon-status";
import { isCompiledExecutable, daemonRelaunchCmd } from "../daemon-spawn";

export type { DaemonState } from "../daemon-status";

export type SpawnDaemon = (cmd: string[], opts: { logPath: string }) => { pid: number };

const defaultSpawnDaemon: SpawnDaemon = (cmd, { logPath }) => {
  const fd = openSync(logPath, "a");
  const proc = Bun.spawn({ cmd, detached: true, stdio: ["ignore", fd, fd] });
  closeSync(fd);
  proc.unref();
  return { pid: proc.pid };
};

const GLOBAL_KEYS = new Set(GLOBAL_FIELDS.map((f) => f.key));

export interface ControllerDeps {
  dbPath: string;
  detect: { claude: typeof detectClaude; git: typeof detectGit };
  db?: Database;
  catalog?: ProviderCatalog;
  dataDir?: string;
  spawnDaemon?: SpawnDaemon;
  killDaemon?: (pid: number, signal: NodeJS.Signals | number) => void;
}

export interface SessionRow {
  sessionPk: string;
  projectId: string;
  status: string;
  title?: string;
  startedBy?: string;
  lastText?: string;
}

export class AppController extends EventEmitter {
  readonly db: Database;
  readonly settings: SettingsStore;
  protected catalog: ProviderCatalog;
  private fieldIndex: Map<string, ConfigField>;
  private dataDir: string;

  constructor(protected deps: ControllerDeps) {
    super();
    this.db = deps.db ?? openDb(deps.dbPath);
    this.settings = new SettingsStore(this.db);
    this.catalog = deps.catalog ?? defaultCatalog;
    this.fieldIndex = new Map(allFields(this.catalog).map((f) => [f.key, f]));
    this.dataDir = deps.dataDir ?? dirname(deps.dbPath);
  }

  protected emitChange(): void {
    this.emit("change");
  }

  get(key: string): string | undefined {
    return this.settings.get(key);
  }
  set(key: string, value: string): void {
    this.settings.set(key, value);
    this.emitChange();
  }
  settingKeys(): string[] {
    return Object.keys(SETTING_DEFS);
  }
  isSecret(key: string): boolean {
    return Boolean(SETTING_DEFS[key]?.secret);
  }
  missingRequired(): string[] {
    return missingRequiredSettings(this.settings, this.catalog);
  }
  isConfigured(): boolean {
    return isConfiguredFn(this.settings, this.catalog);
  }

  field(key: string): ConfigField | undefined {
    return this.fieldIndex.get(key);
  }
  generalFields(): ConfigField[] {
    return allFields(this.catalog).filter((f) => GLOBAL_KEYS.has(f.key) && !f.control);
  }
  gatewayDescriptors(): GatewayDescriptor[] {
    return this.catalog.gateways;
  }
  runtimeDescriptors(): RuntimeDescriptor[] {
    return this.catalog.runtimes;
  }
  gatewayFields(id: string): ConfigField[] {
    return this.catalog.gateway(id)?.fields ?? [];
  }
  runtimeFields(id: string): ConfigField[] {
    return this.catalog.runtime(id)?.fields ?? [];
  }
  enabledGateways(): string[] {
    return csv(this.get("enabled_gateways"));
  }
  enabledRuntimes(): string[] {
    return csv(this.get("enabled_runtimes"));
  }
  defaultRuntime(): string {
    return this.get("default_runtime") ?? "";
  }
  setEnabledGateways(ids: string[]): void {
    this.set("enabled_gateways", ids.join(","));
  }
  setEnabledRuntimes(ids: string[]): void {
    this.set("enabled_runtimes", ids.join(","));
  }
  setDefaultRuntime(id: string): void {
    this.set("default_runtime", id);
  }
  requiredMissingFields(): ConfigField[] {
    return requiredMissingFields(this.settings, this.catalog);
  }
  detectRuntime(id: string): Promise<ToolInfo & { authenticated?: boolean }> {
    return this.catalog.runtime(id)?.detect() ?? Promise.resolve({ found: false });
  }

  async checkEnv(): Promise<{ git: ToolInfo; claude: ToolInfo & { authenticated?: boolean } }> {
    const [git, claude] = await Promise.all([this.deps.detect.git(), this.deps.detect.claude()]);
    return { git, claude };
  }

  daemon(): DaemonState {
    const s = readStatus(this.dataDir);
    const st = deriveState(s, isAlive);
    if (s && !st.running && !st.starting && s.state !== "error") clearStatus(this.dataDir);
    return st;
  }

  logs(): string[] {
    const p = join(this.dataDir, "daemon.log");
    if (!existsSync(p)) return [];
    try {
      return readFileSync(p, "utf8").split("\n").filter(Boolean).slice(-200);
    } catch {
      return [];
    }
  }

  sessions(): SessionRow[] {
    const store = new SessionsStore(this.db);
    return store.list().map((s) => ({
      sessionPk: s.sessionPk,
      projectId: s.projectId,
      status: s.status,
      title: s.title,
      startedBy: s.startedBy,
    }));
  }

  async startDaemon(): Promise<void> {
    const cur = this.daemon();
    if (cur.running || cur.starting) return;
    const missing = this.missingRequired();
    if (missing.length || this.enabledGateways().length === 0) {
      const why = missing.length ? `missing settings: ${missing.join(", ")}` : "no gateways enabled";
      writeStatus(this.dataDir, { pid: -1, state: "error", startedAt: Date.now(), lastError: why });
      this.emitChange();
      return;
    }
    clearStatus(this.dataDir);
    const cmd = daemonRelaunchCmd({
      execPath: process.execPath,
      main: Bun.main,
      compiled: isCompiledExecutable(),
    });
    const spawn = this.deps.spawnDaemon ?? defaultSpawnDaemon;
    spawn(cmd, { logPath: join(this.dataDir, "daemon.log") });
    this.emitChange();
  }

  stopDaemon(): void {
    const s = readStatus(this.dataDir);
    if (s && s.pid > 0 && isAlive(s.pid)) {
      const kill = this.deps.killDaemon ?? ((pid, sig) => process.kill(pid, sig));
      try {
        kill(s.pid, "SIGTERM");
      } catch {
        /* already gone */
      }
    }
    this.emitChange();
  }

  async toggleDaemon(): Promise<void> {
    if (this.daemon().running) this.stopDaemon();
    else await this.startDaemon();
  }
}
