import { create } from "zustand";
import { toast } from "sonner";
import {
  commands,
  type AgentDetailInfo,
  type AgentModelInfo,
  type AgentMutationInfo,
  type AgentRegistryInfo,
  type SelectableModelInfo,
  type Session,
} from "./bindings";
import { LOCAL_RUNNER } from "./lib/session-key";
import { useLearning } from "./store-learning";

// Agent domain store (Plan 3): the YAML agent registry (roster + default +
// subagent model), the focused agent's full detail, and the shared selectable-
// model list. Per-agent Learning UI state lives exclusively in store-learning.

type AgentsState = {
  registry: AgentRegistryInfo | null;
  /** Full detail of the agent currently focused in the detail view. */
  detail: AgentDetailInfo | null;
  /** Provider-driven selectable models, shared by the model pickers. */
  models: SelectableModelInfo[];
  /** True only after a successful registry load. */
  loaded: boolean;
  loading: boolean;
  saving: boolean;
  /** Sessions owned by each stable agent ID, capped by the backend query. */
  recentSessionsByAgent: Record<string, Session[]>;
  loadRecentSessions: (agentId: string) => Promise<void>;
  load: (agentId?: string) => Promise<void>;
  loadDetail: (agentId: string) => Promise<void>;
  create: (input: AgentMutationInfo) => Promise<AgentDetailInfo | null>;
  update: (agentId: string, input: AgentMutationInfo) => Promise<boolean>;
  duplicate: (agentId: string) => Promise<AgentDetailInfo | null>;
  remove: (agentId: string) => Promise<boolean>;
  setDefault: (agentId: string) => Promise<boolean>;
  updateSubagentModel: (model: AgentModelInfo) => Promise<boolean>;
};

/** Patch one roster entry in place (identity-preserving for the rest). */
function patchRoster(registry: AgentRegistryInfo, agentId: string, detail: AgentDetailInfo): AgentRegistryInfo {
  return { ...registry, agents: registry.agents.map((agent) => (agent.id === agentId ? detail.summary : agent)) };
}

/** Insert or replace one roster entry by its stable agent ID. */
function upsertRoster(registry: AgentRegistryInfo, detail: AgentDetailInfo): AgentRegistryInfo {
  const index = registry.agents.findIndex((agent) => agent.id === detail.summary.id);
  if (index === -1) return { ...registry, agents: [...registry.agents, detail.summary] };
  return {
    ...registry,
    agents: registry.agents.map((agent, agentIndex) => (agentIndex === index ? detail.summary : agent)),
  };
}

/** Patch only fields represented by a mutation, preserving server-derived fields. */
function patchRosterFields(registry: AgentRegistryInfo, agentId: string, input: AgentMutationInfo): AgentRegistryInfo {
  return {
    ...registry,
    agents: registry.agents.map((agent) =>
      agent.id === agentId
        ? {
            ...agent,
            name: input.name,
            description: input.description,
            avatarColor: input.avatarColor,
            model: input.model,
            permissionMode: input.permissionMode,
          }
        : agent,
    ),
  };
}

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export const useAgents = create<AgentsState>((set, get) => {
  let loadGeneration = 0;
  let detailGeneration = 0;
  let pendingReads = 0;
  let pendingMutations = 0;
  let mutationTail: Promise<void> = Promise.resolve();
  let registryRevision = 0;
  let detailRevision = 0;

  const bumpRegistryRevision = () => {
    registryRevision += 1;
  };
  const bumpDetailRevision = () => {
    detailRevision += 1;
  };

  const beginRead = () => {
    pendingReads += 1;
    set({ loading: true });
  };
  const finishRead = () => {
    pendingReads -= 1;
    set({ loading: pendingReads > 0 });
  };
  const fenceReads = () => {
    loadGeneration += 1;
    detailGeneration += 1;
  };
  const enqueueMutation = <T>(operation: () => Promise<T>): Promise<T> => {
    pendingMutations += 1;
    set({ saving: true });
    const result = mutationTail.then(operation, operation);
    mutationTail = result.then(
      () => undefined,
      () => undefined,
    );
    return result.finally(() => {
      pendingMutations -= 1;
      set({ saving: pendingMutations > 0 });
    });
  };

  const runLoad = async (agentId: string | undefined, generation: number, requestedDetail: number | null) => {
    try {
      const [registry, models, detail] = await Promise.all([
        commands.listAgents(LOCAL_RUNNER),
        commands.listSelectableModels(LOCAL_RUNNER),
        agentId ? commands.getAgent(LOCAL_RUNNER, agentId) : Promise.resolve(null),
      ]);
      if (loadGeneration !== generation) return;
      if (registry.status === "ok") {
        bumpRegistryRevision();
        set({ registry: registry.data, loaded: true });
      } else toast.error(`Couldn't load agents: ${registry.error.message}`);
      if (models.status === "ok") set({ models: models.data });
      else toast.error(`Couldn't load models: ${models.error.message}`);
      if (agentId && detail && detailGeneration === requestedDetail) {
        if (detail.status === "ok") {
          bumpDetailRevision();
          set({ detail: detail.data });
        } else toast.error(`Couldn't load agent: ${detail.error.message}`);
      }
    } catch (error) {
      if (loadGeneration === generation) toast.error(`Couldn't load agents: ${errorMessage(error)}`);
    }
  };

  const runDetailLoad = async (agentId: string, generation: number) => {
    try {
      const result = await commands.getAgent(LOCAL_RUNNER, agentId);
      if (detailGeneration !== generation) return;
      if (result.status === "ok") {
        bumpDetailRevision();
        set({ detail: result.data });
      } else toast.error(`Couldn't load agent: ${result.error.message}`);
    } catch (error) {
      if (detailGeneration === generation) toast.error(`Couldn't load agent: ${errorMessage(error)}`);
    }
  };

  const snapshotRevisions = (includeDetail = false) => ({
    registry: registryRevision,
    detail: includeDetail ? detailRevision : null,
  });
  const rollbackResources = (
    revisions: ReturnType<typeof snapshotRevisions>,
    snapshot: { registry?: AgentRegistryInfo | null; detail?: AgentDetailInfo | null },
  ) => {
    if (snapshot.registry !== undefined && registryRevision === revisions.registry) set({ registry: snapshot.registry });
    if (snapshot.detail !== undefined && revisions.detail !== null && detailRevision === revisions.detail) {
      set({ detail: snapshot.detail });
    }
  };

  return {
    registry: null,
    detail: null,
    models: [],
    loaded: false,
    loading: false,
    saving: false,
    recentSessionsByAgent: {},

    loadRecentSessions: async (agentId) => {
      try {
        const result = await commands.listAgentSessions(LOCAL_RUNNER, agentId, 10);
        if (result.status === "ok") set((state) => ({ recentSessionsByAgent: { ...state.recentSessionsByAgent, [agentId]: result.data } }));
        else toast.error(`Couldn't load recent sessions: ${result.error.message}`);
      } catch (error) {
        toast.error(`Couldn't load recent sessions: ${errorMessage(error)}`);
      }
    },

    load: async (agentId) => {
      const generation = ++loadGeneration;
      const requestedDetail = agentId ? ++detailGeneration : null;
      beginRead();
      try {
        await runLoad(agentId, generation, requestedDetail);
      } finally {
        finishRead();
      }
    },

    loadDetail: async (agentId) => {
      const generation = ++detailGeneration;
      beginRead();
      try {
        await runDetailLoad(agentId, generation);
      } finally {
        finishRead();
      }
    },

    create: (input) =>
      enqueueMutation(async () => {
        fenceReads();
        try {
          const result = await commands.createAgent(LOCAL_RUNNER, input);
          if (result.status === "error") {
            toast.error(`Create agent failed: ${result.error.message}`);
            return null;
          }
          fenceReads();
          const registry = get().registry;
          set({
            detail: result.data,
            registry: registry ? upsertRoster(registry, result.data) : registry,
          });
          return result.data;
        } catch (error) {
          toast.error(`Create agent failed: ${errorMessage(error)}`);
          return null;
        }
      }),

    update: (agentId, input) =>
      enqueueMutation(async () => {
        fenceReads();
        const previous = { registry: get().registry, detail: get().detail };
        const affectsDetail = previous.detail?.summary.id === agentId;
        const optimisticRevisions = snapshotRevisions(affectsDetail);
        const optimistic: AgentDetailInfo | null =
          previous.detail?.summary.id === agentId
            ? {
                ...previous.detail,
                summary: {
                  ...previous.detail.summary,
                  name: input.name,
                  description: input.description,
                  avatarColor: input.avatarColor,
                  model: input.model,
                  permissionMode: input.permissionMode,
                },
                permissionRules: input.permissionRules,
                skills: input.skills,
                nativeTools: input.nativeTools,
                pluginTools: input.pluginTools,
                apps: input.apps,
              }
            : previous.detail;
        set({
          detail: optimistic,
          registry: previous.registry
            ? optimistic?.summary.id === agentId
              ? patchRoster(previous.registry, agentId, optimistic)
              : patchRosterFields(previous.registry, agentId, input)
            : previous.registry,
        });
        try {
          const result = await commands.updateAgent(LOCAL_RUNNER, agentId, input);
          if (result.status === "error") {
            rollbackResources(optimisticRevisions, previous);
            toast.error(`Update agent failed: ${result.error.message}`);
            return false;
          }
          fenceReads();
          const registry = get().registry;
          set({
            detail: get().detail?.summary.id === agentId ? result.data : get().detail,
            registry: registry ? patchRoster(registry, agentId, result.data) : registry,
          });
          return true;
        } catch (error) {
          rollbackResources(optimisticRevisions, previous);
          toast.error(`Update agent failed: ${errorMessage(error)}`);
          return false;
        }
      }),

    duplicate: (agentId) =>
      enqueueMutation(async () => {
        fenceReads();
        try {
          const result = await commands.duplicateAgent(LOCAL_RUNNER, agentId);
          if (result.status === "error") {
            toast.error(`Duplicate agent failed: ${result.error.message}`);
            return null;
          }
          fenceReads();
          const registry = get().registry;
          set({ registry: registry ? upsertRoster(registry, result.data) : registry });
          return result.data;
        } catch (error) {
          toast.error(`Duplicate agent failed: ${errorMessage(error)}`);
          return null;
        }
      }),

    remove: (agentId) =>
      enqueueMutation(async () => {
        fenceReads();
        try {
          const result = await commands.deleteAgent(LOCAL_RUNNER, agentId);
          if (result.status === "error") {
            toast.error(`Delete agent failed: ${result.error.message}`);
            return false;
          }
          fenceReads();
          useLearning.getState().evictAgent(agentId);
          set((state) => ({
            registry: result.data,
            detail: state.detail?.summary.id === agentId ? null : state.detail,
          }));
          return true;
        } catch (error) {
          toast.error(`Delete agent failed: ${errorMessage(error)}`);
          return false;
        }
      }),

    setDefault: (agentId) =>
      enqueueMutation(async () => {
        fenceReads();
        const previous = get().registry;
        const optimisticRevisions = snapshotRevisions();
        set({
          registry: previous
            ? {
                ...previous,
                defaultAgentId: agentId,
                agents: previous.agents.map((agent) => ({ ...agent, isDefault: agent.id === agentId })),
              }
            : previous,
        });
        try {
          const result = await commands.setDefaultAgent(LOCAL_RUNNER, agentId);
          if (result.status === "error") {
            rollbackResources(optimisticRevisions, { registry: previous });
            toast.error(`Default agent failed: ${result.error.message}`);
            return false;
          }
          fenceReads();
          set({ registry: result.data });
          return true;
        } catch (error) {
          rollbackResources(optimisticRevisions, { registry: previous });
          toast.error(`Default agent failed: ${errorMessage(error)}`);
          return false;
        }
      }),

    updateSubagentModel: (model) =>
      enqueueMutation(async () => {
        fenceReads();
        const previous = get().registry;
        const optimisticRevisions = snapshotRevisions();
        set({ registry: previous ? { ...previous, subagentModel: model } : previous });
        try {
          const result = await commands.updateSubagentModel(LOCAL_RUNNER, model);
          if (result.status === "error") {
            rollbackResources(optimisticRevisions, { registry: previous });
            toast.error(`Subagent model failed: ${result.error.message}`);
            return false;
          }
          fenceReads();
          set({ registry: result.data });
          return true;
        } catch (error) {
          rollbackResources(optimisticRevisions, { registry: previous });
          toast.error(`Subagent model failed: ${errorMessage(error)}`);
          return false;
        }
      }),
  };
});
