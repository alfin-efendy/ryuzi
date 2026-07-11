import type { Page } from "@playwright/test";
import type { ConnectionInfo } from "../src/bindings";

/**
 * Fixtures mirror the generated types in src/bindings.ts (Project, Session,
 * ConnectionInfo).
 * Keep field names in sync when bindings regenerate.
 */
export const PROJECT = {
  projectId: "p-demo",
  name: "demo",
  workdir: "/tmp/demo",
  source: null,
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
    quotaCapability: null,
    models: ["model-alpha", "model-beta"],
    needsRelogin: false,
  },
] satisfies ConnectionInfo[];

export const ACCOUNT_CATALOG = [
  {
    id: "anthropic-oauth",
    name: "Claude Code",
    family: "anthropic",
    color: "#D97757",
    initial: "C",
    category: "oauth",
    format: "anthropic",
    requiresBaseUrl: false,
    models: ["claude-sonnet-4"],
    freeTier: false,
    riskNotice: false,
    usesDeviceGrant: false,
  },
  {
    id: "openai-oauth",
    name: "Codex",
    family: "openai",
    color: "#10A37F",
    initial: "O",
    category: "oauth",
    format: "openai",
    requiresBaseUrl: false,
    models: ["gpt-5.5"],
    freeTier: false,
    riskNotice: false,
    usesDeviceGrant: false,
  },
  {
    id: "kiro",
    name: "Kiro",
    family: "kiro",
    color: "#7C3AED",
    initial: "K",
    category: "device",
    format: "openai",
    requiresBaseUrl: false,
    models: ["kiro-auto"],
    freeTier: true,
    riskNotice: false,
    usesDeviceGrant: false,
  },
];

export const ACCOUNT_CONNECTIONS = [
  {
    id: "claude-personal",
    provider: "anthropic-oauth",
    providerName: "Claude Code",
    color: "#D97757",
    initial: "C",
    authType: "oauth",
    label: "Claude Personal",
    priority: 0,
    enabled: true,
    quotaCapability: "claude",
    models: ["claude-sonnet-4"],
    needsRelogin: true,
  },
  {
    id: "codex-primary",
    provider: "openai-oauth",
    providerName: "Codex",
    color: "#10A37F",
    initial: "O",
    authType: "oauth",
    label: "Codex Primary",
    priority: 0,
    enabled: true,
    quotaCapability: "codex",
    models: ["gpt-5.5"],
    needsRelogin: false,
  },
  {
    id: "codex-backup",
    provider: "openai-oauth",
    providerName: "Codex",
    color: "#10A37F",
    initial: "O",
    authType: "oauth",
    label: "Codex Backup",
    priority: 1,
    enabled: true,
    quotaCapability: "codex",
    models: ["gpt-5.5"],
    needsRelogin: false,
  },
  {
    id: "kiro-device",
    provider: "kiro",
    providerName: "Kiro",
    color: "#7C3AED",
    initial: "K",
    authType: "oauth",
    label: "Kiro Device",
    priority: 0,
    enabled: true,
    quotaCapability: null,
    models: ["kiro-auto"],
    needsRelogin: true,
  },
] satisfies ConnectionInfo[];

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

const initialSessionRuntime = {
  sessionPk: "c-1",
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
  branchOwned: false,
  permMode: "default",
  kind: "project",
  speaker: null,
  agent: null,
  parentSessionPk: null,
};

/** A project-less chat session (kind "chat"), returned by start_chat_session. */
export const CHAT_SESSION = {
  ...SESSION,
  sessionPk: "c-1",
  projectId: null,
  branch: null,
  kind: "chat",
};

export const PROVIDER_FAMILY_ROUTE_SELECTIONS = [
  {
    requestedModel: "route:primary",
    resolvedProviderId: "fixture-provider-a",
    resolvedFamily: "fixture-family-a",
    resolvedModel: "shared-model",
    effectiveEffort: "high",
    connectionId: "fixture-account",
    resolvedModelDisplayName: "Shared Model",
    effectiveEffortLabel: "High",
    connectionLabel: "Fixture account",
    reason: "initial",
  },
  {
    requestedModel: "route:provider-family-change",
    resolvedProviderId: "fixture-provider-b",
    resolvedFamily: "fixture-family-b",
    resolvedModel: "shared-model",
    effectiveEffort: "high",
    connectionId: "fixture-account",
    resolvedModelDisplayName: "Shared Model",
    effectiveEffortLabel: "High",
    connectionLabel: "Fixture account",
    reason: "roundRobin",
  },
  {
    requestedModel: "route:mutable-alias-only",
    resolvedProviderId: "fixture-provider-b",
    resolvedFamily: "fixture-family-b",
    resolvedModel: "shared-model",
    effectiveEffort: "high",
    connectionId: "fixture-account",
    resolvedModelDisplayName: "Renamed Shared Model",
    effectiveEffortLabel: "High (renamed)",
    connectionLabel: "Renamed account label",
    reason: "quotaUnavailable",
  },
];

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
  get_agent_settings: { model: null, permMode: "ask" },
  list_selectable_models: NATIVE_RUNTIME.selectableModels,
  list_runtimes: [NATIVE_RUNTIME],
  refresh_runtimes: [NATIVE_RUNTIME],
  list_gateways: [],
  probe_gateways: [],
  list_jobs: [],
  list_apps: [],
  // Plugin-distribution commands invoked on the Plugins view mount. Without
  // these, the fallback returns `null` for the non-`list_`-prefixed ones
  // (`plugin_doctor`, `plugins_restart_required`), and the store then renders
  // `doctorFindings`/`restartRequired` from `null` — crashing the view and
  // wedging sidebar navigation.
  list_plugins: [],
  list_skills: [],
  plugin_doctor: [],
  plugins_restart_required: false,
  // The Browse tab's status line calls `catalog_status` on Plugins-view
  // mount (not just `list_`/`refresh_`-prefixed calls, which already
  // fall back to `[]` above) — without a fixture the unmocked-command
  // fallback returns `null`, and the store renders `catalogStatus` from
  // `null`, crashing the view. `refresh_catalog` shares the same
  // `CatalogStatus` shape.
  catalog_status: { sequence: 0, lastFetchAt: null, outcome: null, entries: 0, blocked: 0 },
  refresh_catalog: { sequence: 0, lastFetchAt: null, outcome: null, entries: 0, blocked: 0 },
  // An extension-capable plugin's detail view calls `extension_status` on
  // mount (Track D observability, DT8) — same "without a fixture the
  // unmocked fallback returns `null` and the view crashes" lesson as
  // `catalog_status` above. Empty list is a safe default (no plugin in these
  // fixtures declares an `extension` capability).
  extension_status: [],
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
  start_chat_session: CHAT_SESSION,
  session_runtime_info: initialSessionRuntime,
  update_session_runtime: initialSessionRuntime,
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
        resolvedProviderId: string;
        resolvedFamily: string;
        resolvedModel: string;
        effectiveEffort: string | null;
        connectionId: string;
      };
      type RouteSelection = RouteIdentity & {
        requestedModel: string | null;
        resolvedModelDisplayName: string;
        effectiveEffortLabel: string | null;
        connectionLabel: string;
        reason: string;
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
      let connections = fixtures.list_connections as ConnectionInfo[];
      const quotaAttempts = new Map<string, number>();
      const pendingQuota = new Map<string, (value: unknown) => void>();
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
      let sessionRuntime = fixtures.session_runtime_info as {
        sessionPk: string;
        model: string | null;
        storedEffort: string | null;
        effectiveEffort: string | null;
        effectiveEffortLabel: string | null;
        effectiveSource: string;
        storedEffortStatus: string;
        modelInfo: (typeof SELECTABLE_MODELS)[number] | null;
      };

      const resolveRuntime = (owner: { projectId: string } | { sessionPk: string }, model: string | null, effort: string | null) => {
        const modelInfo = (fixtures.list_selectable_models as typeof SELECTABLE_MODELS).find((entry) => entry.requestValue === model);
        const fallback = modelInfo?.supported.find((option) => option.value === modelInfo.resolvedDefault) ?? null;
        const selected = modelInfo?.supported.find((option) => option.value === effort) ?? fallback;
        return {
          ...owner,
          model,
          storedEffort: effort,
          effectiveEffort: selected?.value ?? null,
          effectiveEffortLabel: selected?.label ?? null,
          effectiveSource: effort ? "project" : modelInfo ? "provider" : "none",
          storedEffortStatus: "valid",
          modelInfo: modelInfo ?? null,
        };
      };
      let cbId = 1;
      const eventHandlers = new Map<string, number[]>();
      const w = window as unknown as Record<string, unknown>;
      w.__mockCalls = calls;
      w.__resolveMockQuota = (id: string) => {
        pendingQuota.get(id)?.(quotaFor(id, 99));
        pendingQuota.delete(id);
      };

      const quotaFor = (id: string, usedOverride?: number) => ({
        provider: id.startsWith("claude") ? "anthropic-oauth" : "openai-oauth",
        plan: id.startsWith("claude") ? "Claude Pro" : "ChatGPT Plus",
        message: null,
        limitReached: false,
        reviewLimitReached: false,
        resetCredits: id.startsWith("codex") ? { availableCount: 2, refreshAt: "2030-01-01T00:00:00Z" } : null,
        quotas: [
          {
            label: id.startsWith("claude") ? "5 hour" : "Codex primary",
            usedPercentage: usedOverride ?? (id.endsWith("backup") ? 35 : 20),
            remainingPercentage: 100 - (usedOverride ?? (id.endsWith("backup") ? 35 : 20)),
            resetAt: "2030-01-01T00:00:00Z",
          },
        ],
      });

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
        const scripted = fixtures.route_selections as RouteSelection[] | undefined;
        const currentSelection: RouteSelection = scripted?.[Math.min(durable.routeRequests, scripted.length - 1)] ?? {
          requestedModel: projectRuntime.model,
          resolvedProviderId: "fixture",
          resolvedFamily: modelInfo?.preferenceKey?.family ?? "fixture",
          resolvedModel: modelInfo?.preferenceKey?.model ?? projectRuntime.model ?? "default",
          effectiveEffort: projectRuntime.effectiveEffort,
          connectionId: useBackup ? "fixture-backup" : "fixture-account",
          resolvedModelDisplayName: modelInfo?.displayName ?? projectRuntime.model ?? "Default model",
          effectiveEffortLabel: projectRuntime.effectiveEffortLabel,
          connectionLabel: useBackup ? "Backup account" : "Fixture account",
          reason: useBackup ? "roundRobin" : "initial",
        };
        const current: RouteIdentity = {
          resolvedProviderId: currentSelection.resolvedProviderId,
          resolvedFamily: currentSelection.resolvedFamily,
          resolvedModel: currentSelection.resolvedModel,
          effectiveEffort: currentSelection.effectiveEffort,
          connectionId: currentSelection.connectionId,
        };
        const previous = durable.route;
        durable.route = current;
        durable.routeRequests += 1;

        let text: string | null = null;
        if (previous) {
          const modelChanged =
            previous.resolvedFamily !== current.resolvedFamily ||
            previous.resolvedModel !== current.resolvedModel ||
            previous.effectiveEffort !== current.effectiveEffort;
          const accountChanged =
            previous.resolvedProviderId !== current.resolvedProviderId || previous.connectionId !== current.connectionId;
          const reason =
            {
              ordered: "account order",
              roundRobin: "round robin",
              authenticationUnavailable: "authentication unavailable",
              quotaUnavailable: "quota unavailable",
              rateLimit: "rate limit",
              providerUnavailable: "provider unavailable",
              transportUnavailable: "transport unavailable",
            }[currentSelection.reason] ?? null;
          if (modelChanged) {
            text = `Switched to ${currentSelection.resolvedModelDisplayName}${currentSelection.effectiveEffortLabel ? ` · ${currentSelection.effectiveEffortLabel}` : ""}`;
            if (accountChanged && reason) text += ` via ${currentSelection.connectionLabel} · ${reason}`;
          } else if (accountChanged) {
            text = `Account switched to ${currentSelection.connectionLabel}${reason ? ` · ${reason}` : ""}`;
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
          if (cmd === "list_connections") return Promise.resolve(connections);
          if (cmd === "list_messages") return Promise.resolve(durable.messages);
          if (cmd === "start_session") {
            const session = fixtures.start_session as typeof SESSION;
            sessions = [session];
            persist();
            observeRoute(session.sessionPk);
            return Promise.resolve(session);
          }
          if (cmd === "start_chat_session") {
            const session = fixtures.start_chat_session as typeof CHAT_SESSION;
            const options = (args as { options?: { model?: string | null; effort?: string | null } }).options;
            sessionRuntime = resolveRuntime({ sessionPk: session.sessionPk }, options?.model ?? null, options?.effort ?? null);
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
          if (cmd === "session_runtime_info") return Promise.resolve(sessionRuntime);
          if (cmd === "update_session_runtime") {
            const update = args as { sessionPk: string; model: string | null; effort: string | null };
            sessionRuntime = resolveRuntime({ sessionPk: update.sessionPk }, update.model, update.effort);
            return Promise.resolve(sessionRuntime);
          }
          if (cmd === "connection_provider_quota") {
            const { id } = args as { id: string };
            const attempts = (quotaAttempts.get(id) ?? 0) + 1;
            quotaAttempts.set(id, attempts);
            if (fixtures.quota_failure_once === id && attempts === 1) {
              return Promise.reject({ message: "Provider quota unavailable" });
            }
            const delayKey = `ryuzi.e2e.delayed-quota.${id}`;
            if (fixtures.delayed_quota === id && !localStorage.getItem(delayKey)) {
              localStorage.setItem(delayKey, "pending");
              return new Promise((resolve) => pendingQuota.set(id, resolve));
            }
            return Promise.resolve(quotaFor(id));
          }
          if (cmd === "reset_codex_credit") return Promise.resolve({ consumed: true, availableCount: 1 });
          if (cmd === "rename_connection") {
            const { id, label } = args as { id: string; label: string };
            connections = connections.map((connection) => (connection.id === id ? { ...connection, label } : connection));
            return Promise.resolve(connections);
          }
          if (cmd === "set_connection_enabled") {
            const { id, enabled } = args as { id: string; enabled: boolean };
            connections = connections.map((connection) => (connection.id === id ? { ...connection, enabled } : connection));
            return Promise.resolve(connections);
          }
          if (cmd === "move_connection") {
            const { id, dir } = args as { id: string; dir: number };
            const from = connections.findIndex((connection) => connection.id === id);
            const to = Math.max(0, Math.min(connections.length - 1, from + dir));
            if (from >= 0 && from !== to) {
              const next = [...connections];
              const [moved] = next.splice(from, 1);
              next.splice(to, 0, moved);
              connections = next.map((connection, priority) => ({ ...connection, priority }));
            }
            return Promise.resolve(connections);
          }
          if (cmd === "remove_connection") {
            const { id } = args as { id: string };
            connections = connections.filter((connection) => connection.id !== id);
            return Promise.resolve(connections);
          }
          if (cmd === "test_connection") return Promise.resolve({ ok: true, message: "Connection works" });
          if (cmd === "reconnect_oauth") {
            const { connectionId } = args as { connectionId: string };
            connections = connections.map((connection) =>
              connection.id === connectionId ? { ...connection, needsRelogin: false } : connection,
            );
            return Promise.resolve(connections);
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
