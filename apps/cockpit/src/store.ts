import { create } from "zustand";
import { toast } from "sonner";
import {
  commands,
  events,
  type Project,
  type CoreEvent,
  type Message,
  type ChatRequestOptions,
  type GitOptions,
  type PermMode,
  type ApprovalKind,
  type ApprovalResponse,
  type ModelPreferenceKey,
  type ProjectRuntimeInfo,
  type SessionRuntimeInfo,
  type ModelCost,
  type OrchTask,
  type Principal,
} from "./bindings";
import { basename } from "./lib/paths";
import { useNative } from "./store-native";
import { useAgents } from "./store-agents";
import { useUi } from "./store-ui";
import { messageToRow, mergeToolRow, type Row } from "./lib/transcript";
import { notifier, notifyIntentForEvent, isWindowFocused } from "@/lib/notify";
import { enqueue, dequeue, removeById, type QueuedMessage } from "@/lib/queue";
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
  model?: string | null;
  effort?: string | null;
  context?: {
    branch?: string | null;
    voiceTranscript?: string | null;
    references?: string[];
  } | null;
  attachments?: string[];
  git?: GitOptions | null;
  permMode?: PermMode | null;
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
      outputTokens: number;
    }
  >;
  projectRuntimeById: Record<string, ProjectRuntimeInfo>;
  sessionRuntimeById: Record<string, SessionRuntimeInfo>;
  /** Per-session running cost total + per-model breakdown from the latest `sessionCost` event. */
  sessionCost: Record<string, { totalUsd: number; models: ModelCost[] }>;
  /** Per-session in-memory type-ahead queue (messages typed while running). */
  queued: Record<string, QueuedMessage[]>;
  /** Live orchestration task graph, keyed by root task id — the task strip's
   *  data source. Upserted piecemeal by `orchTaskChanged` events and seeded
   *  in bulk by `loadOrchTasks`. */
  orchTasks: Record<string, OrchTask[]>;
  enqueueMessage: (runnerId: string, sessionPk: string, msg: QueuedMessage) => void;
  removeQueued: (runnerId: string, sessionPk: string, id: string) => void;
  /** Send the head of a session's queue; on send-failure re-queue it at the front. */
  sendNextQueued: (runnerId: string, sessionPk: string) => Promise<void>;
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
  loadSessionRuntime: (runnerId: string, sessionPk: string) => Promise<void>;
  setSessionRuntime: (runnerId: string, sessionPk: string, model: string | null, effort: string | null) => Promise<boolean>;
  /** Change the permission mode this session (only this session) runs under. */
  setSessionPermMode: (runnerId: string, sessionPk: string, permMode: PermMode) => Promise<void>;
  /** Resolves true as soon as the backend accepts — navigate immediately;
   *  the session list refresh completes in the background. `runnerId` is the
   *  runner the session is created on (defaults to the local engine). */
  start: (runnerId: string, projectId: string, prompt: string, options?: ChatOptions | null) => Promise<boolean>;
  /** Same shape as `start`, but for a chat-first session with no project
   *  (`start_chat_session`) — Home's default when no project is attached. */
  startChat: (runnerId: string, prompt: string, options?: ChatOptions | null) => Promise<boolean>;
  /** Resolves true when the backend accepted the prompt — false lets the
   *  composer restore its optimistically-cleared draft. */
  send: (runnerId: string, sessionPk: string, prompt: string, options?: ChatOptions | null) => Promise<boolean>;
  stop: (runnerId: string, sessionPk: string) => Promise<void>;
  /** Resolves true only when the backend teardown actually succeeded. */
  end: (runnerId: string, sessionPk: string) => Promise<boolean>;
  resolveApproval: (runnerId: string, runId: string, requestId: string, response: ApprovalResponse) => Promise<void>;
  hydrateTranscript: (runnerId: string, pk: string, fetcher?: (pk: string) => Promise<Message[]>, force?: boolean) => Promise<void>;
  /** Re-hydrates a session's transcript even when it's already `loaded` —
   *  used after a terminal/blocked `orchTaskChanged` so a report or block
   *  card posted into the home chat lands without a full reload. */
  refetchTranscript: (runnerId: string, pk: string, fetcher?: (pk: string) => Promise<Message[]>) => Promise<void>;
  /** Fetches the full task graph under `rootId` (a fresh strip on mount). */
  loadOrchTasks: (rootId: string) => Promise<void>;
  /** Submit a fresh goal for orchestrated execution (the composer's
   *  "Orchestrate" toggle / `/orchestrate` entry) — bound to the currently
   *  attached project and the currently focused home chat, so worker
   *  bubbles, block-for-human cards, and the aggregate report have somewhere
   *  to post into. Resolves false (a no-op) without both. */
  startOrchestration: (prompt: string, decompose?: boolean) => Promise<boolean>;
  /** Answer a worker's blocking question (BlockCard's inline composer). */
  orchAnswerBlock: (taskId: string, answer: string) => Promise<void>;
  init: () => Promise<void>;
};

function append(map: Record<string, Row[]>, key: string, row: Row): Record<string, Row[]> {
  return { ...map, [key]: [...(map[key] ?? []), row] };
}

function toChatRequestOptions(options?: ChatOptions | null): ChatRequestOptions | null {
  if (!options) return null;
  return {
    model: options.model ?? null,
    effort: options.effort ?? null,
    context: options.context
      ? {
          branch: options.context.branch ?? null,
          voiceTranscript: options.context.voiceTranscript ?? null,
          references: options.context.references ?? [],
        }
      : null,
    attachments: options.attachments ?? [],
    git: options.git ?? null,
    permMode: options.permMode ?? null,
  };
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
const sessionRuntimeLoadGeneration = new Map<string, number>();
const sessionRuntimeQueues = new Map<string, RuntimeQueue<SessionRuntimeInfo>>();

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
  queued: {},
  focusedSession: null,
  selectedProjectId: null,
  lastSeq: {},
  loaded: {},
  contextUsage: {},
  projectRuntimeById: {},
  sessionRuntimeById: {},
  sessionCost: {},
  orchTasks: {},

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
          const row = messageToRow(
            e.seq,
            e.role,
            e.block_type,
            e.payload,
            e.tool_call_id,
            e.status,
            e.tool_kind,
            Date.now(),
            e.session_pk,
            e.speaker,
          );
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
        case "orchTaskChanged": {
          // A root reports its OWN status change with root_id: null (it has
          // no parent) — key the graph by the task's own id in that case.
          const rootId = e.root_id ?? e.task_id;
          const prev = st.orchTasks[rootId] ?? [];
          const idx = prev.findIndex((t) => t.id === e.task_id);
          const next =
            idx >= 0
              ? prev.map((t) => (t.id === e.task_id ? { ...t, status: e.status } : t))
              : [...prev, { id: e.task_id, rootId: e.root_id, status: e.status } as OrchTask];
          // A terminal/blocked change may have posted a bubble or report card
          // into the focused home chat — refetch it so the row lands without
          // waiting for an unrelated refresh.
          if (e.status === "blocked" || e.status === "done" || e.status === "failed") {
            const focused = get().focusedSession;
            if (focused) void get().refetchTranscript(focused.runnerId, focused.pk);
          }
          return { orchTasks: { ...st.orchTasks, [rootId]: next } };
        }
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
      messageToRow(m.seq, m.role, m.blockType, m.payload, m.toolCallId, m.status, m.toolKind, m.createdAt, pk, m.speaker),
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
  // out of band (e.g. an orchestration report posted by a background worker).
  refetchTranscript: (runnerId, pk, fetcher) => get().hydrateTranscript(runnerId, pk, fetcher, true),

  loadOrchTasks: async (rootId) => {
    const res = await commands.orchTasks(rootId);
    if (res.status === "ok") set((st) => ({ orchTasks: { ...st.orchTasks, [rootId]: res.data } }));
  },

  startOrchestration: async (prompt, decompose = true) => {
    const projectId = get().selectedProjectId;
    const home = get().focusedSession;
    if (!projectId || !home) return false;
    const res = await commands.orchSubmit(projectId, prompt, decompose, home.pk);
    if (res.status === "error") toast.error("Couldn't start orchestration: " + res.error.message);
    return res.status === "ok";
  },
  orchAnswerBlock: async (taskId, answer) => {
    const res = await commands.orchAnswerBlock(taskId, answer);
    if (res.status === "error") toast.error("Couldn't send the answer: " + res.error.message);
  },

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
  loadSessionRuntime: async (runnerId, sessionPk) => {
    const loadGeneration = nextGeneration(sessionRuntimeLoadGeneration, sessionPk);
    const queue = sessionRuntimeQueues.get(sessionPk);
    const intent = queue?.latestIntent ?? 0;
    const res = await commands.sessionRuntimeInfo(runnerId, sessionPk);
    if (
      res.status === "ok" &&
      sessionRuntimeLoadGeneration.get(sessionPk) === loadGeneration &&
      (sessionRuntimeQueues.get(sessionPk)?.latestIntent ?? 0) === intent
    ) {
      set((st) => ({ sessionRuntimeById: { ...st.sessionRuntimeById, [sessionPk]: res.data } }));
    }
  },
  setSessionRuntime: async (runnerId, sessionPk, model, effort) => {
    const previousRuntime = get().sessionRuntimeById[sessionPk];
    let queue = sessionRuntimeQueues.get(sessionPk);
    if (!queue) {
      queue = {
        tail: Promise.resolve(),
        pending: 0,
        latestIntent: 0,
        confirmedModel: previousRuntime?.model ?? null,
        confirmedEffort: previousRuntime?.storedEffort ?? null,
        confirmedRuntime: previousRuntime,
      };
      sessionRuntimeQueues.set(sessionPk, queue);
    }
    const intent = ++queue.latestIntent;
    queue.pending += 1;
    const optimistic: SessionRuntimeInfo = previousRuntime
      ? { ...previousRuntime, model, storedEffort: effort }
      : {
          sessionPk,
          model,
          storedEffort: effort,
          effectiveEffort: effort,
          effectiveEffortLabel: effort,
          effectiveSource: effort ? "project" : "none",
          storedEffortStatus: "valid",
          modelInfo: null,
        };
    set((st) => ({ sessionRuntimeById: { ...st.sessionRuntimeById, [sessionPk]: optimistic } }));
    let succeeded = false;
    const execute = async () => {
      try {
        const res = await commands.updateSessionRuntime(runnerId, sessionPk, model, effort);
        succeeded = res.status === "ok";
        if (res.status === "ok") {
          queue.confirmedRuntime = res.data;
          queue.confirmedModel = res.data.model;
          queue.confirmedEffort = res.data.storedEffort;
        } else {
          toast.error("Couldn't set chat model and effort: " + res.error.message);
        }
      } catch (error) {
        toast.error("Couldn't set chat model and effort: " + String(error));
      }
      queue.pending -= 1;
      if (intent === queue.latestIntent) {
        const confirmed = queue.confirmedRuntime;
        set((st) => {
          const next = { ...st.sessionRuntimeById };
          if (confirmed) next[sessionPk] = { ...confirmed, sessionPk };
          else delete next[sessionPk];
          return { sessionRuntimeById: next };
        });
      }
      if (queue.pending === 0) sessionRuntimeQueues.delete(sessionPk);
    };
    const task = queue.pending === 1 ? execute() : queue.tail.then(execute);
    queue.tail = task.catch(() => undefined);
    await task;
    return succeeded;
  },
  setSessionPermMode: async (runnerId, sessionPk, permMode) => {
    const session = get().sessions.find((s) => isSession(s, { runnerId, pk: sessionPk }));
    if (!session || session.permMode === permMode) return;
    set({ sessions: get().sessions.map((s) => (isSession(s, { runnerId, pk: sessionPk }) ? { ...s, permMode } : s)) });
    const res = await commands.updateSessionPermMode(runnerId, sessionPk, permMode);
    if (res.status === "error") {
      toast.error("Couldn't set permission mode: " + res.error.message);
      await get().refresh();
    }
  },

  start: async (runnerId, projectId, prompt, options) => {
    const res = await commands.startSession(runnerId, projectId, prompt, toChatRequestOptions(options));
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
  startChat: async (runnerId, prompt, options) => {
    const res = await commands.startChatSession(runnerId, prompt, toChatRequestOptions(options));
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
  send: async (runnerId, sessionPk, prompt, options) => {
    // A typed message while THIS chat drives a live orchestration steers it
    // instead of running as a normal chat turn — e.g. "cancel" cancels the
    // tree, anything else is noted as guidance for the judge. `orch_steer`
    // returns exactly one of three wire strings: "noted"/"cancelled" when the
    // orchestration actually consumed the message, or "noOrchestration" when
    // there's no live root bound to this session (the store keeps no
    // client-side cache of that binding — the backend check is authoritative).
    // Only the two positive outcomes short-circuit the normal turn; ANYTHING
    // else — "noOrchestration", a thrown IPC error, or an unexpected/null
    // payload from a backend that doesn't implement steering — MUST fall
    // through to the normal send path so a steer-check never swallows the
    // user's message.
    try {
      const steer = await commands.orchSteer(sessionPk, prompt);
      if (steer.status === "ok" && (steer.data === "noted" || steer.data === "cancelled")) return true;
    } catch {
      // fall through to the normal send path
    }
    // A session already RUNNING a turn gets steered — the message is
    // injected into that turn's next tool-result batch instead of racing a
    // whole new turn onto the session. Any other status (idle, interrupted,
    // ended) starts a normal continue. Matched within the correct runner.
    const isRunning = get().sessions.find((s) => isSession(s, { runnerId, pk: sessionPk }))?.status === "running";
    const res = isRunning
      ? await commands.steerSession(runnerId, sessionPk, prompt)
      : await commands.continueSession(runnerId, sessionPk, prompt, toChatRequestOptions(options));
    if (res.status === "error") {
      toast.error("Couldn't send message: " + res.error.message);
    }
    await get().refresh();
    return res.status === "ok";
  },
  enqueueMessage: (runnerId, sessionPk, msg) =>
    set((st) => {
      const key = sessKey(runnerId, sessionPk);
      return { queued: { ...st.queued, [key]: enqueue(st.queued[key], msg) } };
    }),
  removeQueued: (runnerId, sessionPk, id) =>
    set((st) => {
      const key = sessKey(runnerId, sessionPk);
      return { queued: { ...st.queued, [key]: removeById(st.queued[key], id) } };
    }),
  sendNextQueued: async (runnerId, sessionPk) => {
    const key = sessKey(runnerId, sessionPk);
    const { head, rest } = dequeue(get().queued[key]);
    if (!head) return;
    // Remove the head BEFORE awaiting so a second `result` can't re-send it.
    set((st) => ({ queued: { ...st.queued, [key]: rest } }));
    const ok = await get().send(runnerId, sessionPk, head.text, head.options);
    if (!ok) {
      // Command-level rejection: put it back at the front so it stays visible.
      set((st) => ({ queued: { ...st.queued, [key]: [head, ...(st.queued[key] ?? [])] } }));
    }
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
      drainQueueOnEvent(event, runnerId);
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

/**
 * Drain one queued message when a session's turn finishes *successfully*.
 * Keyed on `result` only: an `error` turn emits no `result`, so the queue
 * stays put (structural pause). Extracted so the decision is testable without
 * a real Tauri event subscription.
 */
export function drainQueueOnEvent(event: CoreEvent, runnerId: string): void {
  if (event.kind !== "result") return;
  const pk = (event as { session_pk?: string }).session_pk;
  if (pk) void useStore.getState().sendNextQueued(runnerId, pk);
}
