// apps/router/src/cli/start-command.ts
import { openDb } from "../store/db";
import { ProjectsStore } from "../store/projects";
import { SessionsStore } from "../store/sessions";
import { SettingsStore } from "../config/store";
import { ControlPlane } from "../core/control-plane";
import { Router } from "../core/router";
import { ClaudeCodeHarness } from "../harness/claude-code/index";
import { DiscordGateway, type DiscordPort } from "../gateways/discord/index";
import { startApprovalServer } from "../core/approval-server";
import type { Harness } from "../harness/types";
import type { Telemetry } from "../observability/types";
import { ConsoleTelemetry } from "../observability/console";
import { createOtelTelemetry } from "../observability/otel";
import type { Database } from "bun:sqlite";
import { expandHome } from "../config/paths";

export function buildDaemon(deps: { dbPath: string; port: DiscordPort; harnessFactory?: () => Harness; db?: Database; telemetry?: Telemetry }): {
  gateway: DiscordGateway; cp: ControlPlane; start(): Promise<void>; stop(): void;
} {
  const db = deps.db ?? openDb(deps.dbPath);
  const projects = new ProjectsStore(db);
  const sessions = new SessionsStore(db);
  const settings = new SettingsStore(db);
  const workdirRoot = expandHome(settings.get("workdir_root") ?? ".");

  let telemetry: Telemetry;
  const otelEndpoint = settings.get("otel_endpoint");
  if (deps.telemetry) {
    telemetry = deps.telemetry;
  } else if (otelEndpoint) {
    try {
      telemetry = createOtelTelemetry({ endpoint: otelEndpoint });
    } catch {
      process.stderr.write("[telemetry] OTel init failed — falling back to console\n");
      telemetry = new ConsoleTelemetry();
    }
  } else {
    telemetry = new ConsoleTelemetry();
  }

  const cp = new ControlPlane({ projects, sessions, settings, workdirRoot, telemetry });
  cp.harnesses.register("claude-code", deps.harnessFactory ?? (() => new ClaudeCodeHarness()));
  const router = new Router(cp, sessions, projects);
  const gateway = new DiscordGateway(deps.port, router);
  cp.gateways.register(gateway);
  const ipc = startApprovalServer(cp);
  cp.approvalUrl = ipc.url;
  cp.hookBinPath = `${import.meta.dir}/../hook/pretooluse-bin.ts`;
  return { gateway, cp, start: () => gateway.start(), stop: () => { void telemetry.shutdown?.(); ipc.stop(); } };
}
