import { EventEmitter } from "node:events";
import type { Database } from "bun:sqlite";
import { openDb } from "../../store/db";
import { SettingsStore } from "../../config/store";
import { SETTING_DEFS } from "../../config/schema";
import type { detectClaude, detectGit } from "../../harness/detect";
import type { ToolInfo } from "../../harness/detect";
import type { DiscordPort } from "../../gateways/discord/index";
import type { Harness } from "../../harness/types";
import { buildDaemon } from "../start-command";
import { reduceSessions, type LiveSession } from "./sessions-reducer";
import { SessionsStore } from "../../store/sessions";
import { DiscordClientPort } from "../../gateways/discord/client-port";
import { ClaudeCodeHarness } from "../../harness/claude-code/index";
import type { ControlPlane } from "../../core/control-plane";
import type { CoreEvent, Unsubscribe } from "@harness/protocol";
import type { Telemetry, Span, Attrs } from "../../observability/types";

export interface ControllerDeps {
  dbPath: string;
  detect: { claude: typeof detectClaude; git: typeof detectGit };
  db?: Database;
  portFactory?: (cfg: { token: string; appId: string; guildId: string }) => DiscordPort;
  harnessFactory?: () => Harness;
}

export interface DaemonState { running: boolean; startedAt?: number; lastError?: string; starting?: boolean }
export interface SessionRow { sessionPk: string; projectId: string; status: string; title?: string; startedBy?: string; lastText?: string }

class SinkTelemetry implements Telemetry {
  constructor(private sink: (line: string) => void) {}
  startSpan(_n: string, _a?: Attrs): Span { return { setAttribute() {}, setError: (m) => this.sink("error: " + m), end() {} }; }
  count(name: string, _a?: Attrs): void { this.sink(name); }
  record(_n: string, _v: number, _a?: Attrs): void {}
}

export class AppController extends EventEmitter {
  readonly db: Database;
  readonly settings: SettingsStore;
  private daemonHandle?: { stop(): void };
  private daemonCp?: ControlPlane;
  private daemonState: DaemonState = { running: false };
  private logLines: string[] = [];
  private live = new Map<string, LiveSession>();
  private cpUnsub?: Unsubscribe;

  constructor(protected deps: ControllerDeps) {
    super();
    this.db = deps.db ?? openDb(deps.dbPath);
    this.settings = new SettingsStore(this.db);
  }

  protected emitChange(): void { this.emit("change"); }

  get(key: string): string | undefined { return this.settings.get(key); }
  set(key: string, value: string): void { this.settings.set(key, value); this.emitChange(); }
  settingKeys(): string[] { return Object.keys(SETTING_DEFS); }
  isSecret(key: string): boolean { return Boolean(SETTING_DEFS[key]?.secret); }
  requiredKeys(): string[] { return Object.entries(SETTING_DEFS).filter(([, d]) => d.required).map(([k]) => k); }
  missingRequired(): string[] { return this.settings.missingRequired(); }
  isConfigured(): boolean { return this.missingRequired().length === 0; }

  async checkEnv(): Promise<{ git: ToolInfo; claude: ToolInfo & { authenticated?: boolean } }> {
    const [git, claude] = await Promise.all([this.deps.detect.git(), this.deps.detect.claude()]);
    return { git, claude };
  }

  daemon(): DaemonState { return this.daemonState; }
  logs(): string[] { return this.logLines; }

  private pushLog(line: string): void {
    this.logLines.push(line);
    if (this.logLines.length > 200) this.logLines.shift();
    this.emitChange();
  }

  sessions(): SessionRow[] {
    const store = new SessionsStore(this.db);
    return store.list().map((s) => {
      const live = this.live.get(s.sessionPk);
      return {
        sessionPk: s.sessionPk, projectId: s.projectId,
        status: live?.status ?? s.status, title: s.title,
        startedBy: s.startedBy, lastText: live?.lastText,
      };
    });
  }

  async startDaemon(): Promise<void> {
    if (this.daemonState.running) return;
    const missing = this.missingRequired();
    if (missing.length) {
      this.daemonState = { running: false, lastError: `missing settings: ${missing.join(", ")}` };
      this.emitChange();
      return;
    }
    const cfg = { token: this.get("discord_token")!, appId: this.get("discord_app_id")!, guildId: this.get("discord_guild_id")! };
    const port = this.deps.portFactory ? this.deps.portFactory(cfg) : new DiscordClientPort(cfg);
    const telemetry = new SinkTelemetry((line) => this.pushLog(line));
    const daemon = buildDaemon({
      dbPath: this.deps.dbPath, db: this.db, port,
      harnessFactory: this.deps.harnessFactory ?? (() => new ClaudeCodeHarness()),
      telemetry,
    });
    this.daemonCp = daemon.cp;
    this.cpUnsub = daemon.cp.subscribe((e: CoreEvent) => { reduceSessions(this.live, e); this.emitChange(); });
    this.daemonState = { ...this.daemonState, starting: true, lastError: undefined };
    this.emitChange();
    try {
      await daemon.start();
      this.daemonHandle = daemon;
      this.daemonState = { running: true, startedAt: Date.now(), starting: false };
      this.pushLog("daemon started");
    } catch (e) {
      this.cpUnsub?.(); this.cpUnsub = undefined; this.daemonCp = undefined;
      this.daemonState = { running: false, starting: false, lastError: (e as Error).message };
      this.emitChange();
    }
  }

  stopDaemon(): void {
    this.daemonHandle?.stop();
    this.cpUnsub?.(); this.cpUnsub = undefined; this.daemonCp = undefined; this.daemonHandle = undefined;
    this.daemonState = { ...this.daemonState, running: false, startedAt: undefined };
    this.pushLog("daemon stopped");
  }

  async toggleDaemon(): Promise<void> {
    if (this.daemonState.running) this.stopDaemon(); else await this.startDaemon();
  }
}
