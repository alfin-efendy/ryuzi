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
      const storageKey = "ryuzi.e2e.route-state.v1";
      type MockMessage = {
        sessionPk: string;
        seq: number;
        role: string;
        blockType: string;
        payload: { text: string };
        toolCallId: null;
        status: null;
        toolKind: null;
        createdAt: number;
      };
      type RouteIdentity = {
        model: string | null;
        effort: string | null;
        connectionId: string;
        connectionLabel: string;
      };
      type DurableState = {
        sessions: (typeof SESSION)[];
        messages: MockMessage[];
        route: RouteIdentity | null;
        routeRequests: number;
      };
      const stored = localStorage.getItem(storageKey);
      const durable: DurableState = stored
        ? (JSON.parse(stored) as DurableState)
        : { sessions: fixtures.list_sessions as (typeof SESSION)[], messages: [], route: null, routeRequests: 0 };
      let sessions = durable.sessions;
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
      const eventHandlers = new Map<string, number[]>();
      const w = window as unknown as Record<string, unknown>;
      w.__mockCalls = calls;

      const persist = () => {
        durable.sessions = sessions;
        localStorage.setItem(storageKey, JSON.stringify(durable));
      };

      const emitCoreEvent = (event: Record<string, unknown>) => {
        for (const handler of eventHandlers.get("core-event-msg") ?? []) {
          const callback = (window as unknown as Record<string, (payload: unknown) => void>)[`_${handler}`];
          callback?.({ event: "core-event-msg", id: 0, payload: { event } });
        }
      };

      const observeRoute = (sessionPk: string) => {
        const modelInfo = (fixtures.list_runtimes as (typeof NATIVE_RUNTIME)[])[0].selectableModels.find(
          (model) => model.requestValue === projectRuntime.model,
        );
        const useBackup = durable.routeRequests >= 2;
        const current: RouteIdentity = {
          model: projectRuntime.model,
          effort: projectRuntime.effectiveEffort,
          connectionId: useBackup ? "fixture-backup" : "fixture-account",
          connectionLabel: useBackup ? "Backup account" : "Fixture account",
        };
        const previous = durable.route;
        durable.route = current;
        durable.routeRequests += 1;

        let text: string | null = null;
        if (previous) {
          const modelChanged = previous.model !== current.model || previous.effort !== current.effort;
          const accountChanged = previous.connectionId !== current.connectionId;
          const effortLabel = modelInfo?.supported.find((option) => option.value === current.effort)?.label;
          if (modelChanged) {
            text = `Switched to ${modelInfo?.displayName ?? current.model ?? "Default model"}${effortLabel ? ` · ${effortLabel}` : ""}`;
          } else if (accountChanged) {
            text = `Account switched to ${current.connectionLabel} · round robin`;
          }
        }

        if (text) {
          const message: MockMessage = {
            sessionPk,
            seq: durable.messages.length + 1,
            role: "system",
            blockType: "notice",
            payload: { text },
            toolCallId: null,
            status: null,
            toolKind: null,
            createdAt: Date.now(),
          };
          durable.messages.push(message);
          persist();
          emitCoreEvent({
            kind: "message",
            session_pk: message.sessionPk,
            seq: message.seq,
            role: message.role,
            block_type: message.blockType,
            payload: message.payload,
            tool_call_id: message.toolCallId,
            status: message.status,
            tool_kind: message.toolKind,
          });
        } else {
          persist();
        }
      };

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
          if (cmd === "plugin:event|listen") {
            const registration = args as { event: string; handler: number };
            eventHandlers.set(registration.event, [...(eventHandlers.get(registration.event) ?? []), registration.handler]);
            return Promise.resolve(registration.handler);
          }
          if (cmd === "plugin:event|unlisten") return Promise.resolve(null);
          if (cmd.startsWith("plugin:")) return Promise.resolve(null);
          if (cmd === "list_sessions") return Promise.resolve(sessions);
          if (cmd === "list_messages") return Promise.resolve(durable.messages);
          if (cmd === "start_session") {
            const session = fixtures.start_session as typeof SESSION;
            sessions = [session];
            persist();
            observeRoute(session.sessionPk);
            return Promise.resolve(session);
          }
          if (cmd === "continue_session") {
            observeRoute((args as { sessionPk: string }).sessionPk);
            return Promise.resolve(null);
          }
          if (cmd === "stop_session") {
            const { sessionPk } = args as { sessionPk: string };
            sessions = sessions.map((session) => (session.sessionPk === sessionPk ? { ...session, status: "idle" as const } : session));
            persist();
            return Promise.resolve(null);
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
