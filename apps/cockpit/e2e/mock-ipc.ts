import type { Page } from "@playwright/test";
import type {
  AgentDetailInfo,
  AgentRegistryInfo,
  AgentRun,
  AgentSummaryInfo,
  ConnectionInfo,
  Message,
  ModelRouteTargetCapability,
  Session,
} from "../src/bindings";

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

export const SESSION = {
  sessionPk: "s-1",
  primaryAgentId: "ryuzi",
  primaryAgentSnapshot: { id: "ryuzi", name: "Ryuzi", avatarColor: "#7C3AED" },
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

/** Two main agents for the agent-management, delegation, and read-only
 * history journeys: Ryuzi (the executable default) and Reviewer (a second
 * executable agent used as a delegation target and, in the history journey,
 * a session owner deliberately absent from a narrower registry override). */
export const RYUZI_AGENT = {
  id: "ryuzi",
  name: "Ryuzi",
  description: "General-purpose coding agent",
  avatarColor: "violet",
  model: { kind: "concrete", name: "fixture/model-alpha", effort: "high" },
  permissionMode: "ask",
  skillCount: 1,
  toolCount: 4,
  knowledgeCount: 1,
  executable: true,
  validation: [],
  isDefault: true,
} satisfies AgentSummaryInfo;

export const REVIEWER_AGENT = {
  id: "reviewer",
  name: "Reviewer",
  description: "Reviews implementation quality and regressions",
  avatarColor: "amber",
  model: { kind: "route", route: "safe" },
  permissionMode: "ask",
  skillCount: 1,
  toolCount: 4,
  knowledgeCount: 1,
  executable: true,
  validation: [],
  isDefault: false,
} satisfies AgentSummaryInfo;

const AGENT_REGISTRY = {
  agents: [RYUZI_AGENT, REVIEWER_AGENT],
  defaultAgentId: RYUZI_AGENT.id,
  recovery: [],
  subagentModel: { kind: "route", route: "smart" },
} satisfies AgentRegistryInfo;

/** Same registry with Reviewer removed — the deleted-owner history journey
 * needs a session whose captured `primaryAgentId` no longer resolves against
 * the live roster (session-primary.ts's "deleted" branch). */
export const REGISTRY_WITHOUT_REVIEWER = {
  ...AGENT_REGISTRY,
  agents: [RYUZI_AGENT],
} satisfies AgentRegistryInfo;

export const RYUZI_DETAIL = {
  summary: RYUZI_AGENT,
  permissionRules: [],
  skills: ["general-coding"],
  nativeTools: ["read_file", "grep"],
  pluginTools: [],
  apps: [],
  maxTurns: 40,
  maxToolRounds: 12,
  modelInfo: null,
} satisfies AgentDetailInfo;

export const REVIEWER_DETAIL = {
  summary: REVIEWER_AGENT,
  permissionRules: [],
  skills: ["code-review"],
  nativeTools: ["read_file", "grep"],
  pluginTools: [],
  apps: [],
  maxTurns: 40,
  maxToolRounds: 12,
  modelInfo: null,
} satisfies AgentDetailInfo;

/** `get_agent` is dispatched dynamically by `agentId` (see installMockIPC's
 * `get_agent` branch below) — unlike the fixed-shape commands in FIXTURES,
 * this bag is looked up by id at call time. */
const AGENT_DETAILS: Record<string, AgentDetailInfo> = {
  ryuzi: RYUZI_DETAIL,
  reviewer: REVIEWER_DETAIL,
};

/** One active main-delegate run (Ryuzi → Reviewer) and one completed subagent
 * run, returned by `get_child_runs` for the delegation/child-transcript
 * journey. Subagents are ephemeral runtime workers with no agent profile
 * (AgentsView's SubagentSettings: "Subagents do not have profiles"), so
 * `executingAgentId` stays null for the subagent row. */
export const DELEGATE_ACTIVE_RUN = {
  runId: "run-active-1",
  sessionPk: CHAT_SESSION.sessionPk,
  parentRunId: null,
  retryOf: null,
  primaryAgentId: "ryuzi",
  executingAgentId: "reviewer",
  executingAgentNameSnapshot: "Reviewer",
  agentKind: "main-delegate",
  task: "Review the diff for regressions",
  status: "running",
  startedAt: 0,
  finishedAt: null,
  toolCount: 2,
  resolvedModel: "fixture/model-alpha",
  resolvedEffort: "high",
  result: null,
  error: null,
} satisfies AgentRun;

export const DELEGATE_DONE_RUN = {
  runId: "run-done-1",
  sessionPk: CHAT_SESSION.sessionPk,
  parentRunId: null,
  retryOf: null,
  primaryAgentId: "ryuzi",
  executingAgentId: null,
  executingAgentNameSnapshot: "Subagent worker",
  agentKind: "subagent",
  task: "Run the test suite",
  status: "completed",
  startedAt: 0,
  finishedAt: 30_000,
  toolCount: 5,
  resolvedModel: "fixture/model-beta",
  resolvedEffort: null,
  result: "All tests passed.",
  error: null,
} satisfies AgentRun;

export const REVIEWER_CHILD_TRANSCRIPT = [
  {
    sessionPk: CHAT_SESSION.sessionPk,
    seq: 1,
    role: "assistant",
    blockType: "text",
    payload: { text: "Reviewing the diff for regressions now." },
    toolCallId: null,
    status: null,
    toolKind: null,
    createdAt: 0,
    speaker: null,
  },
] satisfies Message[];

/** The parent chat session's own transcript, seeded so the delegation
 * journey can prove the main transcript survives a Right Panel round trip
 * (open the Reviewer child run, then Back). */
export const DELEGATION_PARENT_MESSAGE = {
  sessionPk: CHAT_SESSION.sessionPk,
  seq: 1,
  role: "assistant",
  blockType: "text",
  payload: { text: "Kicking off the review delegation." },
  toolCallId: null,
  status: null,
  toolKind: null,
  createdAt: 0,
  speaker: null,
} satisfies Message;

/** Legacy (no captured owner) and deleted-owner (captured owner absent from
 * the current registry) sessions for the read-only history journey. Both are
 * chat-first (`kind: "chat"`) and idle so `composeReadOnly` in SessionView is
 * driven purely by session-primary.ts's ownership logic, not by `running`. */
export const LEGACY_SESSION = {
  ...SESSION,
  sessionPk: "s-legacy",
  primaryAgentId: null,
  primaryAgentSnapshot: null,
  projectId: null,
  branch: null,
  kind: "chat",
  status: "idle",
  title: "Legacy history",
  permMode: "default",
} satisfies Session;

export const DELETED_OWNER_SESSION = {
  ...SESSION,
  sessionPk: "s-deleted",
  primaryAgentId: "reviewer",
  primaryAgentSnapshot: { id: "reviewer", name: "Reviewer", avatarColor: "amber" },
  projectId: null,
  branch: null,
  kind: "chat",
  status: "idle",
  title: "Deleted owner history",
  permMode: "default",
} satisfies Session;

export const LEGACY_MESSAGE = {
  sessionPk: "s-legacy",
  seq: 1,
  role: "assistant",
  blockType: "text",
  payload: { text: "This is the preserved legacy transcript." },
  toolCallId: null,
  status: null,
  toolKind: null,
  createdAt: 0,
  speaker: null,
} satisfies Message;

export const DELETED_OWNER_MESSAGE = {
  sessionPk: "s-deleted",
  seq: 1,
  role: "assistant",
  blockType: "text",
  payload: { text: "This is the preserved reviewer transcript." },
  toolCallId: null,
  status: null,
  toolKind: null,
  createdAt: 0,
  speaker: null,
} satisfies Message;

/** Route target capabilities for the route-effort journey: model-alpha
 * supports an explicit "high" override, model-beta supports none. Keyed to
 * match PROVIDER_CATALOG/CONNECTIONS' "fixture" family models above, so the
 * Route tab's target picker (routeTargetOptions) resolves two real targets
 * without any per-test override. */
export const ROUTE_TARGET_CAPABILITIES = [
  { provider: "fixture", model: "model-alpha", supported: [{ value: "high", label: "High", description: null }], providerDefault: null },
  { provider: "fixture", model: "model-beta", supported: [], providerDefault: null },
] satisfies ModelRouteTargetCapability[];

/** Tauri command → resolved value (Result-typed commands get the raw data). */
const FIXTURES: Record<string, unknown> = {
  list_projects: [PROJECT],
  list_sessions: [],
  list_messages: [],
  list_agents: AGENT_REGISTRY,
  refresh_agents: [],
  list_providers: [],
  list_provider_catalog: PROVIDER_CATALOG,
  list_connections: CONNECTIONS,
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
  list_model_routes: [],
  list_model_route_target_capabilities: ROUTE_TARGET_CAPABILITIES,
  // Not session-scoped in this mock (get_child_runs/get_child_transcript take
  // a sessionPk but the fixture always answers with the same value) — empty
  // by default so a test that never dispatches has an empty roster; the
  // delegation journey overrides both per-test.
  get_child_runs: [],
  get_child_transcript: [],
  // Internal lookup bag for the dynamically-dispatched `get_agent` command
  // (see its branch in installMockIPC) — not a real Tauri command name.
  agent_details: AGENT_DETAILS,
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
      // Command names Plan 6 (agentic cleanup) permanently deleted from the
      // Tauri invoke surface — the single-agent settings/memory/Learning/
      // curator/orch commands, and the `learning_cmd` trio. If the UI ever
      // calls one of these again (a regression `check-agentic-cleanup.ts`
      // can't catch, since it only scans source text, not runtime
      // invocations), the unmocked-command fallback below throws instead of
      // silently resolving — so an accidental call fails the test
      // immediately rather than degrading quietly.
      const removedCommands = new Set([
        "search_sessions",
        "list_skill_usage",
        "set_skill_pinned",
        "get_agent_settings",
        "set_agent_settings",
        "read_memory",
        "write_memory",
        "learning_graph",
        "curator_status",
        "curator_rollback",
        "orch_submit",
        "orch_list_roots",
        "orch_tasks",
        "orch_cancel",
        "orch_retry",
        "orch_answer_block",
        "orch_steer",
      ]);
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
        speaker: null;
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
        modelRoutes: Array<{
          id: string;
          name: string;
          enabled: boolean;
          strategy: string;
          targets: Array<{ provider: string; model: string; effort: string | null }>;
          createdAt: number;
          updatedAt: number;
        }>;
      };
      const stored = localStorage.getItem(storageKey);
      const durable: DurableState = stored
        ? (JSON.parse(stored) as DurableState)
        : {
            sessions: fixtures.list_sessions as (typeof SESSION)[],
            // Seeds pre-existing history (e.g. legacy/deleted-owner
            // transcripts) — most fixtures start empty and grow only via
            // observeRoute's route-switch notices.
            messages: (fixtures.list_messages as MockMessage[] | undefined) ?? [],
            route: null,
            routeRequests: 0,
            modelRoutes: fixtures.list_model_routes as DurableState["modelRoutes"],
          };
      let sessions = durable.sessions;
      let connections = fixtures.list_connections as ConnectionInfo[];
      let modelRoutes = durable.modelRoutes;
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
        durable.modelRoutes = modelRoutes;
        localStorage.setItem(storageKey, JSON.stringify(durable));
      };

      const emitCoreEvent = (event: Record<string, unknown>) => {
        // The real CoreEventMsg envelope is `{ runnerId, event }` (bindings.ts) —
        // store.ts's listener destructures both and keys per-session state by
        // `sessKey(runnerId, session_pk)`. All fixture sessions here are started
        // on the local runner (see LOCAL_RUNNER in src/lib/session-key.ts), so
        // the mock must stamp the same id or the live event lands under a
        // different composite key than the one the UI is reading from.
        for (const handler of eventHandlers.get("core-event-msg") ?? []) {
          const callback = (window as unknown as Record<string, (payload: unknown) => void>)[`_${handler}`];
          callback?.({ event: "core-event-msg", id: 0, payload: { runnerId: "local", event } });
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
            speaker: null,
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
          if (cmd === "list_messages") {
            const { sessionPk } = args as { sessionPk: string };
            return Promise.resolve(durable.messages.filter((message) => message.sessionPk === sessionPk));
          }
          if (cmd === "get_agent") {
            const { agentId } = args as { agentId: string };
            const details = fixtures.agent_details as Record<string, unknown>;
            return Promise.resolve(details[agentId] ?? null);
          }
          if (cmd === "list_model_routes") return Promise.resolve(modelRoutes);
          if (cmd === "save_model_route") {
            const { route } = args as { route: (typeof modelRoutes)[number] };
            modelRoutes = modelRoutes.some((current) => current.id === route.id)
              ? modelRoutes.map((current) => (current.id === route.id ? route : current))
              : [...modelRoutes, route];
            persist();
            return Promise.resolve(modelRoutes);
          }
          if (cmd === "start_session") {
            const session = fixtures.start_session as typeof SESSION;
            sessions = [session];
            persist();
            observeRoute(session.sessionPk);
            return Promise.resolve(session);
          }
          if (cmd === "start_chat_session") {
            const session = fixtures.start_chat_session as typeof CHAT_SESSION;
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
          if (removedCommands.has(cmd)) {
            throw new Error(`[mock-ipc] removed command invoked by UI: ${cmd} — this should never be called`);
          }
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
