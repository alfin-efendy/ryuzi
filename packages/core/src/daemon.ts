// packages/core/src/daemon.ts
import { openDb } from "./store/db";
import { ProjectsStore } from "./store/projects";
import { SessionsStore } from "./store/sessions";
import { SettingsStore } from "./config/store";
import { ControlPlane } from "./core/control-plane";
import { Router } from "./core/router";
import { startApprovalServer } from "./core/approval-server";
import type { Gateway } from "./gateways/types";
import type { Telemetry } from "./observability/types";
import { ConsoleTelemetry } from "./observability/console";
import { createOtelTelemetry } from "./observability/otel";
import type { Database } from "bun:sqlite";
import { expandHome } from "./config/paths";
import { catalog as defaultCatalog } from "./providers/catalog";
import type { ProviderCatalog } from "./providers/types";
import { csv } from "./config/required";

export function buildDaemon(deps: { dbPath: string; db?: Database; telemetry?: Telemetry; catalog?: ProviderCatalog }): {
  gateways: Gateway[];
  cp: ControlPlane;
  start(): Promise<void>;
  stop(): Promise<void>;
} {
  const cat = deps.catalog ?? defaultCatalog;
  const db = deps.db ?? openDb(deps.dbPath);
  const projects = new ProjectsStore(db);
  const sessions = new SessionsStore(db);
  const settings = new SettingsStore(db);
  const workdirRoot = expandHome(settings.get("workdir_root") ?? ".");

  let telemetry: Telemetry;
  const otelEndpoint = settings.get("otel_endpoint");
  if (deps.telemetry) telemetry = deps.telemetry;
  else if (otelEndpoint) {
    try {
      telemetry = createOtelTelemetry({ endpoint: otelEndpoint });
    } catch {
      process.stderr.write("[telemetry] OTel init failed — falling back to console\n");
      telemetry = new ConsoleTelemetry();
    }
  } else telemetry = new ConsoleTelemetry();

  const cp = new ControlPlane({ projects, sessions, settings, workdirRoot, telemetry });
  for (const id of csv(settings.get("enabled_runtimes"))) {
    const d = cat.runtime(id);
    if (d) cp.harnesses.register(id, () => d.build());
  }
  const router = new Router(cp, sessions, projects);
  const gateways: Gateway[] = [];
  for (const id of csv(settings.get("enabled_gateways"))) {
    const d = cat.gateway(id);
    if (!d) continue;
    const cfg = Object.fromEntries(d.fields.map((f) => [f.key, settings.get(f.key) ?? ""]));
    const gw = d.build(cfg, { router });
    cp.gateways.register(gw);
    gateways.push(gw);
  }
  const ipc = startApprovalServer(cp);
  cp.approvalUrl = ipc.url;
  cp.hookBinPath = `${import.meta.dir}/hook/pretooluse-bin.ts`;
  return {
    gateways,
    cp,
    start: () => Promise.all(gateways.map((g) => g.start())).then(() => {}),
    stop: async () => {
      await Promise.all(gateways.map((g) => g.stop?.()));
      void telemetry.shutdown?.();
      ipc.stop();
    },
  };
}
