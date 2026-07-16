import { create } from "zustand";
import { toast } from "sonner";
import {
  commands,
  events,
  type Project,
  type CoreEvent,
  type Message,
  type TurnInput,
  type GitOptions,
  type ApprovalKind,
  type ApprovalResponse,
  type ModelPreferenceKey,
  type ModelCost,
  type Principal,
  type ProjectRuntimeInfo,
} from "./bindings";
import { basename } from "./lib/paths";
import { useDelegation } from "./store-delegation";
import { useNative } from "./store-native";
import { useAgents } from "./store-agents";
import { useUi } from "./store-ui";
import { messageToRow, mergeToolRow, type Row } from "./lib/transcript";
import { notifier, notifyIntentForEvent, isWindowFocused } from "@/lib/notify";
import { LOCAL_RUNNER, sessKey, refKey, isSession, sameRef, refOf, type SessionRef, type UiSession } from "@/lib/session-key";

export type PendingApproval = {
  runnerId: string;
  sessionPk: string;
  /** Durable agent run that owns this request; required to resolve it. */
  runId: string;
  requestId: string;
  tool: string;
  summary: string;
  kind: ApprovalKind;
  input: unknown;
  /** Which plugin's MCP tool this approval is for; `null` for built-in tools
   *  and Plan/Question prompts. Attribution only — never gates the decision. */
  principal: Principal | null;
};
export type ChatOptions = {
  context?: {
    branch?: string | null;
    voiceTranscript?: string | null;
    references?: string[];
  } | null;
  mentions?: TurnInput["mentions"];
  attachments?: string[];
  git?: GitOptions | null;
};

type State = {
  projects: Project[];
  sessions: UiSession[];
  /** Per-session state, keyed by `sessKey(runnerId, session_pk)` — session pks
   *  collide across runners (each its own DB), so the runner MUST be part of
   *  the key. */
  transcripts: Record<string, Row[]>;
  pendingApprovals: PendingApproval[];
  focusedSession: SessionRef | null;
  selectedProjectId: string | null;
  lastSeq: Record<string, number>;
  loaded: Record<string, boolean>;
  /** Per-session context-window usage from the latest `contextUsage` event. */
  contextUsage: Record<
    string,
    {
      activeTokens: number;
      usableWindow: number;
      percentLeft: number;
      contextWindow: number;
      cacheReadTokens: number;
      cacheCreationTokens: number;
      outputTokens: number;
    }
  >;
  projectRuntimeById: Record<string, ProjectRuntimeInfo>;
  /** Per-session running cost total + per-model breakdown from the latest `sessionCost` event. */
  sessionCost: Record<string, { totalUsd: number; models: ModelCost[] }>;
  /** `runnerId` is the runner that produced the event (from the CoreEventMsg
   *  wrapper). */
  applyCoreEvent: (e: CoreEvent, runnerId: string) => void;
  clearApproval: (runId: string, requestId: string) => void;
  setFocused: (ref: SessionRef | null) => void;
  selectProject: (id: string | null) => void;
  refresh: () => Promise<void>;
  /** Native folder picker → connect_project. True when a project was added. */
  addProject: () => Promise<boolean>;
  /** Clone `url` into `<destParent>/<repo-name>` via the backend. */
  cloneProject: (url: string, destParent: string) => Promise<boolean>;
  loadProjectRuntime: (projectId: string) => Promise<void>;
  setProjectRuntime: (projectId: string, model: string | null, effort: string | null) => Promise<boolean>;
  setModelEffortPreference: (key: ModelPreferenceKey, effort: string | null) => Promise<boolean>;
  refreshModelConfiguration: () => Promise<void>;
  /** Resolves true as soon as the backend accepts — navigate immediately;
   *  the session list refresh completes in the background. `runnerId` is the
   *  runner the session is created on (defaults to the local engine). */
  start: (runnerId: string, projectId: string, primaryAgentId: string, turn: TurnInput) => Promise<boolean>;
  /** Same shape as `start`, but for a chat-first session with no project. */
  startChat: (runnerId: string, primaryAgentId: string, turn: TurnInput) => Promise<boolean>;
  /** Resolves true when the backend accepts an ownership-preserving turn. */
  send: (runnerId: string, sessionPk: string, turn: TurnInput) => Promise<boolean>;
  stop: (runnerId: string, sessionPk: string) => Promise<void>;
  /** Resolves true only when the backend teardown actually succeeded. */
  end: (runnerId: string, sessionPk: string) => Promise<boolean>;
  resolveApproval: (runnerId: string, runId: string, requestId: string, response: ApprovalResponse) => Promise<void>;
  hydrateTranscript: (runnerId: string, pk: string, fetcher?: (pk: string) => Promise<Message[]>, force?: boolean) => Promise<void>;
  refetchTranscript: (runnerId: string, pk: string, fetcher?: (pk: string) => Promise<Message[]>) => Promise<void>;
  /** Fetches the full task graph under `rootId` (a fresh strip on mount). */
  init: () => Promise<void>;
};

function append(map: Record<string, Row[]>, key: string, row: Row): Record<string, Row[]> {
  return { ...map, [key]: [...(map[key] ?? []), row] };
}

let modelConfigurationGeneration = 0;
const projectRuntimeMutationGeneration = new Map<string, number>();
const projectRuntimeActiveMutation = new Map<string, number>();
const projectRuntimeLoadGeneration = new Map<string, number>();

type RuntimeQueue<T> = {
  tail: Promise<void>;
  pending: number;
  latestIntent: number;
  confirmedModel: string | null;
  confirmedEffort: string | null;
  confirmedRuntime: T | undefined;
};

const projectRuntimeQueues = new Map<string, RuntimeQueue<ProjectRuntimeInfo>>();

function nextGeneration(generations: Map<string, number>, projectId: string): number {
  const generation = (generations.get(projectId) ?? 0) + 1;
  generations.set(projectId, generation);
  return generation;
}

export const useStore = create<State>((set, get) => ({
  projects: [],
  sessions: [],
  transcripts: {},
  pendingApprovals: [],
  focusedSession: null,
  selectedProjectId: null,
  lastSeq: {},
  loaded: {},
  contextUsage: {},
  projectRuntimeById: {},
  sessionCost: {},

  applyCoreEvent: (e, runnerId) =>
    set((st) => {
      // The single composite key used for EVERY per-session map touched below.
      // Non-session events (jobRunChanged/oauth/…) fall through the switch's
      // default before this is meaningfully used.
      const key = "session_pk" in e ? sessKey(runnerId, e.session_pk) : "";
      switch (e.kind) {
        case "sessionCreated":
          return {
            transcripts: { ...st.transcripts, [key]: st.transcripts[key] ?? [] },
            loaded: { ...st.loaded, [key]: true },
          };
        case "message": {
          // A COMPLETED `todowrite` means this session's todo list just changed
          // in the DB — refetch so the plan widget updates mid-run instead of
          // waiting for the run to settle. (The initial in_progress emit fires
          // BEFORE the tool executes, so fetching then would read stale data.)
          // Fire-and-forget: loadTodos' fetch token drops out-of-order replies.
          const payload = (e.payload ?? {}) as Record<string, unknown>;
          if (e.block_type === "tool_call" && payload.name === "todowrite" && e.status === "completed") {
            void useNative.getState().loadTodos(runnerId, e.session_pk);
          }
          // CORRUPTION-CRITICAL: the tool-upsert lookup, the high-water
          // read/write, the append, and hydrateTranscript ALL use `key`
          // (the composite runner+pk). If any diverged, tool updates would
          // land in one bucket while appends land in another — silent
          // transcript corruption.
          // Tool updates re-use the original row's seq — upsert by identity
          // BEFORE the seq high-water guard would drop them as stale.
          if (e.tool_call_id) {
            const rows = st.transcripts[key] ?? [];
            const idx = rows.findIndex((r) => r.toolCallId === e.tool_call_id);
            if (idx >= 0) {
              const next = rows.slice();
              next[idx] = mergeToolRow(rows[idx], e.payload, e.status, e.tool_kind);
              return { transcripts: { ...st.transcripts, [key]: next } };
            }
          }
          const prev = st.lastSeq[key] ?? 0;
          if (e.seq <= prev) return {}; // stale/duplicate (covers reload/replay races)
          const row = messageToRow(e.seq, e.role, e.block_type, e.payload, e.tool_call_id, e.status, e.tool_kind, Date.now(), e.session_pk);
          return {
            transcripts: append(st.transcripts, key, row),
            lastSeq: { ...st.lastSeq, [key]: e.seq },
          };
        }
        case "error":
          // Turn failed. The error text is persisted by the backend
          // (emit_error) and arrives as a normal "message" event
          // (role=system, block_type=error) BEFORE this terminal event, so
          // appending a transient copy here would double-render it. Mirror
          // the "result" arm: flip the session out of "running" (composer
          // leaves Stop mode, the "Working…" pulse stops) and refresh so the
          // DB-side Running→Idle demotion lands in the UI.
          void get().refresh();
          return {
            sessions: st.sessions.map((s) => (isSession(s, { runnerId, pk: e.session_pk }) ? { ...s, status: "idle" as const } : s)),
          };
        case "approvalRequested":
          return {
            pendingApprovals: [
              ...st.pendingApprovals,
              {
                runnerId,
                sessionPk: e.session_pk,
                runId: e.run_id,
                requestId: e.request_id,
                tool: e.tool,
                summary: e.summary,
                kind: e.approval_kind,
                input: e.input,
                principal: e.principal ?? null,
              },
            ],
          };
        case "result":
          // Turn finished — the session is alive but awaiting input. Flip it out of "running"
          // so the composer leaves Stop mode and the user can reply.
          // Also: turn-end guarantees the background git/harness prep (branch, worktreePath)
          // has already backfilled the DB row — refresh now so the UI picks it up instead
          // of waiting for some unrelated action to call refresh().
          void get().refresh();
          return {
            sessions: st.sessions.map((s) => (isSession(s, { runnerId, pk: e.session_pk }) ? { ...s, status: "idle" as const } : s)),
          };
        case "sessionEnded":
          return {
            sessions: st.sessions.map((s) => (isSession(s, { runnerId, pk: e.session_pk }) ? { ...s, status: "ended" as const } : s)),
          };
        case "contextUsage":
          return {
            contextUsage: {
              ...st.contextUsage,
              [key]: {
                activeTokens: e.active_tokens,
                usableWindow: e.usable_window,
                percentLeft: e.percent_left,
                contextWindow: e.context_window,
                cacheReadTokens: e.cache_read_tokens,
                cacheCreationTokens: e.cache_creation_tokens,
                outputTokens: e.output_tokens,
              },
            },
          };
        case "sessionCost":
          return {
            sessionCost: {
              ...st.sessionCost,
              [key]: { totalUsd: e.total_usd, models: e.models },
            },
          };
        case "contextCompacted":
          // The transcript notice arrives as a persisted message row; no
          // extra state to keep here.
          return {};
        case "agentRunChanged":
        case "agentRunMessage":
          useDelegation.getState().applyCoreEvent(e, runnerId);
          return {};
        default:
          return {};
      }
    }),

  clearApproval: (runId, requestId) =>
    set((st) => ({
      pendingApprovals: st.pendingApprovals.filter((a) => a.runId !== runId || a.requestId !== requestId),
    })),

  setFocused: (ref) => {
    const prev = get().focusedSession;
    if (prev && !sameRef(prev, ref)) {
      const prevSession = get().sessions.find((s) => isSession(s, prev));
      if (prevSession) useUi.getState().markRead(refKey(prev), prevSession.lastActive ?? 0);
    }
    set({ focusedSession: ref });
    if (ref) notifier.cancelSettle(ref.runnerId, ref.pk);
    if (ref && !get().loaded[refKey(ref)]) void get().hydrateTranscript(ref.runnerId, ref.pk);
  },

  hydrateTranscript: async (runnerId, pk, fetcher, force = false) => {
    const key = sessKey(runnerId, pk);
    if (!force && get().loaded[key]) return;
    const rows = fetcher
      ? await fetcher(pk)
      : await (async () => {
          const res = await commands.listMessages(runnerId, pk);
          return res.status === "ok" ? res.data : [];
        })();
    const hydrated = rows.map((m) =>
      messageToRow(m.seq, m.role, m.blockType, m.payload, m.toolCallId, m.status, m.toolKind, m.createdAt, pk),
    );
    const maxSeq = rows.reduce((mx, m) => Math.max(mx, m.seq), 0);
    set((st) => {
      // Rows appended by applyCoreEvent while listMessages was in flight
      // (fresher than the snapshot) must survive the replace. seq-0 rows are
      // kept defensively — no reducer currently produces them. Same composite
      // `key` as the message reducer — see the CORRUPTION-CRITICAL note there.
      const liveTail = (st.transcripts[key] ?? []).filter((r) => r.seq > maxSeq || r.seq === 0);
      return {
        transcripts: { ...st.transcripts, [key]: [...hydrated, ...liveTail] },
        lastSeq: { ...st.lastSeq, [key]: Math.max(st.lastSeq[key] ?? 0, maxSeq) },
        loaded: { ...st.loaded, [key]: true },
      };
    });
  },

  // Same fetch as hydrateTranscript, but forced — bypasses the `loaded`
  // short-circuit so a session already on screen picks up a row that landed
  // out-of-band transcript refresh for newly persisted messages.
  refetchTranscript: (runnerId, pk, fetcher) => get().hydrateTranscript(runnerId, pk, fetcher, true),

  // Selecting a project clears the focused session so the center shows the "start a new session" composer.
  selectProject: (id) => set({ selectedProjectId: id, focusedSession: null }),

  refresh: async () => {
    // Fan out across every reachable runner: the local engine (always present)
    // plus any connected remote gateways. Each runner is its own DB, so its
    // sessions are stamped with the runner id that produced them and merged
    // into one flat list. Projects come from the local engine.
    const projects = await commands.listProjects(LOCAL_RUNNER);
    if (projects.status === "ok") set({ projects: projects.data });

    const runnerIds = new Set<string>([LOCAL_RUNNER]);
    const gateways = await commands.listGateways(LOCAL_RUNNER);
    if (gateways.status === "ok") {
      for (const g of gateways.data) {
        // The local gateway is already covered by LOCAL_RUNNER; only pull in
        // remote runners we can actually reach.
        if (g.kind !== "local" && g.status === "connected") runnerIds.add(g.id);
      }
    }

    const perRunner = await Promise.all(
      [...runnerIds].map(async (runnerId): Promise<UiSession[]> => {
        const res = await commands.listSessions(runnerId, null);
        return res.status === "ok" ? res.data.map((s) => ({ ...s, runnerId })) : [];
      }),
    );
    const sessions = perRunner.flat();
    set({ sessions });
    useUi.getState().seedReadState(sessions);
  },

  addProject: async () => {
    const dir = await commands.pickDirectory();
    if (!dir) return false;
    const name = basename(dir) || "project";
    // Projects are managed on the local engine.
    const res = await commands.connectProject(LOCAL_RUNNER, dir, name);
    if (res.status === "ok") {
      await get().refresh();
      return true;
    }
    toast.error("Couldn't add project: " + res.error.message);
    return false;
  },

  cloneProject: async (url, destParent) => {
    const res = await commands.cloneProject(LOCAL_RUNNER, url, destParent);
    if (res.status === "ok") {
      await get().refresh();
      return true;
    }
    toast.error("Couldn't clone project: " + res.error.message);
    return false;
  },

  loadProjectRuntime: async (projectId) => {
    const mutationGeneration = projectRuntimeMutationGeneration.get(projectId) ?? 0;
    const startedDuringMutation = projectRuntimeActiveMutation.has(projectId);
    const loadGeneration = nextGeneration(projectRuntimeLoadGeneration, projectId);
    // Projects are managed on the local engine.
    const res = await commands.projectRuntimeInfo(LOCAL_RUNNER, projectId);
    if (
      res.status === "ok" &&
      !startedDuringMutation &&
      !projectRuntimeActiveMutation.has(projectId) &&
      (projectRuntimeMutationGeneration.get(projectId) ?? 0) === mutationGeneration &&
      projectRuntimeLoadGeneration.get(projectId) === loadGeneration
    ) {
      set((st) => ({ projectRuntimeById: { ...st.projectRuntimeById, [projectId]: res.data } }));
    }
  },
  setProjectRuntime: async (projectId, model, effort) => {
    const project = get().projects.find((p) => p.projectId === projectId);
    if (!project) return false;
    const previousRuntime = get().projectRuntimeById[projectId];
    let queue = projectRuntimeQueues.get(projectId);
    if (!queue) {
      queue = {
        tail: Promise.resolve(),
        pending: 0,
        latestIntent: 0,
        confirmedModel: project.model,
        confirmedEffort: project.effort,
        confirmedRuntime: previousRuntime,
      };
      projectRuntimeQueues.set(projectId, queue);
    }
    const intent = ++queue.latestIntent;
    queue.pending += 1;
    const mutationGeneration = nextGeneration(projectRuntimeMutationGeneration, projectId);
    projectRuntimeActiveMutation.set(projectId, mutationGeneration);
    const optimisticRuntime: ProjectRuntimeInfo = previousRuntime
      ? { ...previousRuntime, model, storedEffort: effort }
      : {
          projectId,
          model,
          storedEffort: effort,
          effectiveEffort: effort,
          effectiveEffortLabel: effort,
          effectiveSource: effort ? "project" : "none",
          storedEffortStatus: "valid",
          modelInfo: null,
        };
    set((st) => ({
      projects: st.projects.map((p) => (p.projectId === projectId ? { ...p, model, effort } : p)),
      projectRuntimeById: { ...st.projectRuntimeById, [projectId]: optimisticRuntime },
    }));
    let succeeded = false;
    const execute = async () => {
      try {
        const res = await commands.updateProjectRuntime(LOCAL_RUNNER, projectId, model, effort);
        succeeded = res.status === "ok";
        if (res.status === "ok") {
          queue.confirmedRuntime = res.data;
          queue.confirmedModel = res.data.model;
          queue.confirmedEffort = res.data.storedEffort;
        } else {
          toast.error("Couldn't set model and effort: " + res.error.message);
        }
      } catch (error) {
        toast.error("Couldn't set model and effort: " + String(error));
      }
      queue.pending -= 1;
      if (intent === queue.latestIntent) {
        projectRuntimeActiveMutation.delete(projectId);
        set((st) => {
          const projectRuntimeById = { ...st.projectRuntimeById };
          if (queue.confirmedRuntime) projectRuntimeById[projectId] = queue.confirmedRuntime;
          else delete projectRuntimeById[projectId];
          return {
            projects: st.projects.map((candidate) =>
              candidate.projectId === projectId ? { ...candidate, model: queue.confirmedModel, effort: queue.confirmedEffort } : candidate,
            ),
            projectRuntimeById,
          };
        });
      }
      if (queue.pending === 0) projectRuntimeQueues.delete(projectId);
    };
    const task = queue.pending === 1 ? execute() : queue.tail.then(execute);
    queue.tail = task.catch(() => undefined);
    await task;
    return succeeded;
  },
  refreshModelConfiguration: async () => {
    const generation = ++modelConfigurationGeneration;
    await useAgents.getState().load();
    const projectIds = Object.keys(get().projectRuntimeById);
    const projectMutationSnapshots = new Map(
      projectIds.map((id) => [
        id,
        {
          generation: projectRuntimeMutationGeneration.get(id) ?? 0,
          active: projectRuntimeActiveMutation.has(id),
        },
      ]),
    );
    const entries = await Promise.all(projectIds.map(async (id) => [id, await commands.projectRuntimeInfo(LOCAL_RUNNER, id)] as const));
    if (generation !== modelConfigurationGeneration) return;
    set((st) => {
      const next = { ...st.projectRuntimeById };
      for (const [id, result] of entries) {
        const snapshot = projectMutationSnapshots.get(id);
        if (
          result.status === "ok" &&
          snapshot &&
          !snapshot.active &&
          !projectRuntimeActiveMutation.has(id) &&
          (projectRuntimeMutationGeneration.get(id) ?? 0) === snapshot.generation
        ) {
          next[id] = result.data;
        }
      }
      return { projectRuntimeById: next };
    });
  },
  setModelEffortPreference: async (key, effort) => {
    const res = await commands.setModelEffortPreference(LOCAL_RUNNER, key, effort);
    if (res.status === "error") {
      toast.error("Couldn't set model default: " + res.error.message);
      return false;
    }
    await get().refreshModelConfiguration();
    return true;
  },
  start: async (runnerId, projectId, primaryAgentId, turn) => {
    const res = await commands.startSession(runnerId, projectId, primaryAgentId, turn);
    if (res.status === "error") {
      toast.error("Couldn't start session: " + res.error.message);
      return false;
    }
    // Optimistic navigation: the backend returns the session row before its
    // git/harness startup finishes. Seed (stamped with its runner) and focus
    // it now; the full refresh catches up in the background.
    const seeded: UiSession = { ...res.data, runnerId };
    set({ focusedSession: refOf(seeded), sessions: [...get().sessions, seeded] });
    void get().refresh();
    return true;
  },
  startChat: async (runnerId, primaryAgentId, turn) => {
    const res = await commands.startChatSession(runnerId, primaryAgentId, turn);
    if (res.status === "error") {
      toast.error("Couldn't start chat: " + res.error.message);
      return false;
    }
    // Same optimistic-navigation seed as start(): focus the returned row
    // immediately, then let the background refresh catch up.
    const seeded: UiSession = { ...res.data, runnerId };
    set({ focusedSession: refOf(seeded), sessions: [...get().sessions, seeded] });
    void get().refresh();
    return true;
  },
  send: async (runnerId, sessionPk, turn) => {
    // A session already RUNNING a turn gets steered — the message is
    // injected into that turn's next tool-result batch instead of racing a
    // whole new turn onto the session. Any other status (idle, interrupted,
    // ended) starts a normal continue. Matched within the correct runner.
    const isRunning = get().sessions.find((s) => isSession(s, { runnerId, pk: sessionPk }))?.status === "running";
    const res = isRunning
      ? await commands.steerSession(runnerId, sessionPk, turn.text)
      : await commands.continueSession(runnerId, sessionPk, turn);
    if (res.status === "error") {
      toast.error("Couldn't send message: " + res.error.message);
    }
    await get().refresh();
    return res.status === "ok";
  },
  stop: async (runnerId, sessionPk) => {
    const res = await commands.stopSession(runnerId, sessionPk);
    if (res.status === "error") {
      toast.error("Couldn't stop session: " + res.error.message);
    }
    await get().refresh();
  },
  end: async (runnerId, sessionPk) => {
    const res = await commands.endSession(runnerId, sessionPk);
    if (res.status === "error") {
      toast.error("Couldn't end session: " + res.error.message);
    }
    await get().refresh();
    return res.status === "ok";
  },
  resolveApproval: async (runnerId, runId, requestId, response) => {
    try {
      await commands.resolveApproval(runnerId, runId, requestId, response);
      get().clearApproval(runId, requestId);
    } catch (e) {
      console.error("resolveApproval failed", e);
      toast.error("Approval failed: " + String(e));
    }
  },

  init: async () => {
    await get().refresh();
    await events.coreEventMsg.listen((e) => {
      const { runnerId, event } = e.payload;
      get().applyCoreEvent(event, runnerId);
      // Keep the actively-viewed session marked read as its activity streams in.
      markFocusedSessionReadOnEvent(event, runnerId, get().focusedSession);
      // OS notification for attention events (suppressed while focused).
      const intent = notifyIntentForEvent(event, runnerId, isWindowFocused());
      const evtPk = (event as { session_pk?: string }).session_pk;
      if (evtPk) notifier.cancelSettle(runnerId, evtPk); // any activity supersedes a pending settle
      if (intent)
        notifier.handle(
          intent,
          get().sessions.find((s) => isSession(s, { runnerId: intent.runnerId, pk: intent.sessionPk })),
        );
      if (event.kind === "sessionQueueChanged") void useNative.getState().loadQueue(runnerId, event.session_pk);
      // Sessions can be created outside UI actions (e.g. scheduler runs) —
      // refresh the list so they appear in the sidebar immediately.
      if (event.kind === "sessionCreated") void get().refresh();
    });
  },
}));

/**
 * A core event for the session currently focused in the UI counts as the
 * user having "seen" it as it streams in — mark that session read so its
 * unread dot never lags behind what's already on screen. Extracted from the
 * `init()` listener so the decision is testable without driving a real Tauri
 * event subscription.
 */
export function markFocusedSessionReadOnEvent(event: CoreEvent, runnerId: string, focusedSession: SessionRef | null): void {
  const activePk = (event as { session_pk?: string }).session_pk;
  if (activePk && focusedSession && sameRef({ runnerId, pk: activePk }, focusedSession)) {
    useUi.getState().markRead(sessKey(runnerId, activePk), Date.now());
  }
}
