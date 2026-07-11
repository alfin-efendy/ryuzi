import type { Page } from "@playwright/test";

/**
 * Fixtures mirror the generated types in src/bindings.ts (Project, Session).
 * Keep field names in sync when bindings regenerate.
 */
export const PROJECT = {
  projectId: "p-demo",
  name: "demo",
  workdir: "/tmp/demo",
  source: null,
  harness: "claude",
  model: null,
  effort: null,
  permMode: "default",
  createdAt: 0,
  isGit: false,
};

const effort = (value: string, label: string) => ({ value, label, description: `${label} fixture effort` });

export const SELECTABLE_MODELS = [
  {
    kind: "concrete",
    requestValue: "fixture/model-alpha",
    displayName: "Model Alpha",
    preferenceKey: { family: "fixture", model: "model-alpha" },
    supported: [effort("light", "Light"), effort("medium", "Medium"), effort("high", "High")],
    configuredDefault: null,
    resolvedDefault: "medium",
    defaultSource: "provider",
  },
  {
    kind: "concrete",
    requestValue: "fixture/model-beta",
    displayName: "Model Beta",
    preferenceKey: { family: "fixture", model: "model-beta" },
    supported: [effort("high", "High"), effort("extra-high", "Extra high"), effort("ultra", "Ultra")],
    configuredDefault: null,
    resolvedDefault: "extra-high",
    defaultSource: "provider",
  },
  {
    kind: "namedRoute",
    requestValue: "route:safe",
    displayName: "Named safe route",
    preferenceKey: null,
    supported: [effort("high", "High")],
    configuredDefault: null,
    resolvedDefault: "high",
    defaultSource: "variesByTarget",
  },
];

export const NATIVE_RUNTIME = {
  id: "native",
  name: "Ryuzi",
  color: "#8B5CF6",
  initial: "R",
  connection: "In-process",
  binaryPath: "in-process",
  installedVersion: "0.0.0-fixture",
  latestVersion: null,
  npmPackage: null,
  models: SELECTABLE_MODELS.map((model) => model.requestValue),
  selectableModels: SELECTABLE_MODELS,
  enabled: true,
  model: "",
  permMode: "ask",
  flags: "",
  tiers: [],
  isDefault: true,
  runnable: true,
};

const PROVIDER_CATALOG = [
  {
    id: "fixture",
    name: "Fixture Provider",
    family: "fixture",
    color: "#6366F1",
    initial: "F",
    category: "api_key",
    format: "openai",
    requiresBaseUrl: false,
    models: ["model-alpha", "model-beta"],
    freeTier: false,
    riskNotice: false,
    usesDeviceGrant: false,
  },
];

const CONNECTIONS = [
  {
    id: "fixture-account",
    provider: "fixture",
    providerName: "Fixture Provider",
    color: "#6366F1",
    initial: "F",
    authType: "apiKey",
    label: "Fixture account",
    priority: 0,
    enabled: true,
    baseUrl: null,
    models: ["model-alpha", "model-beta"],
    keyMasked: "fixture-…key",
    needsRelogin: false,
    claudeCloaking: false,
  },
];

const initialProjectRuntime = {
  projectId: PROJECT.projectId,
  model: null,
  storedEffort: null,
  effectiveEffort: null,
  effectiveEffortLabel: null,
  effectiveSource: "none",
  storedEffortStatus: "valid",
  modelInfo: null,
};

export const SESSION = {
  sessionPk: "s-1",
  projectId: "p-demo",
  agentSessionId: null,
  worktreePath: null,
  branch: "main",
  title: null,
  status: "running",
  startedBy: null,
  createdAt: 0,
  lastActive: 0,
  resumeAttempts: 0,
};

/** Tauri command → resolved value (Result-typed commands get the raw data). */
const FIXTURES: Record<string, unknown> = {
  list_projects: [PROJECT],
  list_sessions: [],
  list_messages: [],
  list_agents: [],
  refresh_agents: [],
  list_providers: [],
  list_provider_catalog: PROVIDER_CATALOG,
  list_connections: CONNECTIONS,
  list_runtimes: [NATIVE_RUNTIME],
  refresh_runtimes: [NATIVE_RUNTIME],
  list_gateways: [],
  probe_gateways: [],
  list_jobs: [],
  list_apps: [],
  get_setting: null,
  backdrop_capability: "none",
  system_accent_color: null,
  start_session: SESSION,
  project_runtime_info: initialProjectRuntime,
  update_project_runtime: initialProjectRuntime,
  provider_account_route: { provider: "fixture", strategy: "fallback" },
  list_model_statuses: [],
  list_all_model_statuses: [],
  connection_usage: null,
  set_model_effort_preference: null,
};

/**
 * Installs a fake `window.__TAURI_INTERNALS__` before the app boots, so the
 * real `@tauri-apps/api` code path resolves against fixtures instead of a
 * missing Tauri bridge. `plugin:*` invokes (event listen, window show)
 * resolve to null. Every call is recorded on `window.__mockCalls`.
 */
export async function installMockIPC(page: Page, overrides: Record<string, unknown> = {}): Promise<void> {
  await page.addInitScript(
    (fixtures) => {
      const calls: Array<{ cmd: string; args: unknown }> = [];
      let sessions = fixtures.list_sessions as (typeof SESSION)[];
      let projectRuntime = fixtures.project_runtime_info as {
        projectId: string;
        model: string | null;
        storedEffort: string | null;
        effectiveEffort: string | null;
        effectiveEffortLabel: string | null;
        effectiveSource: string;
        storedEffortStatus: string;
        modelInfo: (typeof SELECTABLE_MODELS)[number] | null;
      };
      let cbId = 1;
      const w = window as unknown as Record<string, unknown>;
      w.__mockCalls = calls;
      w.__TAURI_INTERNALS__ = {
        metadata: {
          currentWindow: { label: "main" },
          currentWebview: { label: "main", windowLabel: "main" },
        },
        plugins: {},
        transformCallback: (cb: (payload: unknown) => void) => {
          const id = cbId++;
          Object.defineProperty(window, `_${id}`, { value: cb, configurable: true });
          return id;
        },
        invoke: (cmd: string, args: unknown) => {
          calls.push({ cmd, args });
          if (cmd.startsWith("plugin:")) return Promise.resolve(null);
          if (cmd === "list_sessions") return Promise.resolve(sessions);
          if (cmd === "start_session") {
            const session = fixtures.start_session as typeof SESSION;
            sessions = [session];
            return Promise.resolve(session);
          }
          if (cmd === "project_runtime_info") return Promise.resolve(projectRuntime);
          if (cmd === "update_project_runtime") {
            const update = args as { model: string | null; effort: string | null };
            const modelInfo = (fixtures.list_runtimes as (typeof NATIVE_RUNTIME)[])[0].selectableModels.find(
              (model) => model.requestValue === update.model,
            );
            const fallback = modelInfo?.supported.find((option) => option.value === modelInfo.resolvedDefault) ?? null;
            const selected = modelInfo?.supported.find((option) => option.value === update.effort) ?? fallback;
            projectRuntime = {
              projectId: projectRuntime.projectId,
              model: update.model,
              storedEffort: update.effort,
              effectiveEffort: selected?.value ?? null,
              effectiveEffortLabel: selected?.label ?? null,
              effectiveSource: update.effort ? "project" : modelInfo ? "provider" : "none",
              storedEffortStatus: "valid",
              modelInfo: modelInfo ?? null,
            };
            return Promise.resolve(projectRuntime);
          }
          if (cmd in fixtures) return Promise.resolve(fixtures[cmd]);
          console.warn("[mock-ipc] unmocked command:", cmd);
          if (cmd.startsWith("list_") || cmd.startsWith("refresh_") || cmd.startsWith("probe_")) {
            return Promise.resolve([]);
          }
          return Promise.resolve(null);
        },
      };
    },
    { ...FIXTURES, ...overrides },
  );
}

export async function mockCalls(page: Page): Promise<Array<{ cmd: string; args: Record<string, unknown> | undefined }>> {
  return page.evaluate(() => (window as unknown as { __mockCalls: [] }).__mockCalls);
}
