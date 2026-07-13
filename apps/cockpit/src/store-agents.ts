import { create } from "zustand";
import { toast } from "sonner";
import {
  commands,
  type AgentDetailInfo,
  type AgentModelInfo,
  type AgentMutationInfo,
  type AgentRegistryInfo,
  type SelectableModelInfo,
} from "./bindings";
import { LOCAL_RUNNER } from "./lib/session-key";

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
  return { ...registry, agents: registry.agents.map((a) => (a.id === agentId ? detail.summary : a)) };
}

/** Optimistically patch only the mutation-representable summary fields of one
 *  roster entry. Used when the focused detail belongs to a different agent, so
 *  the target row still previews the edit without borrowing another agent's
 *  summary (or any server-derived fields like counts and validation). */
function patchRosterFields(registry: AgentRegistryInfo, agentId: string, input: AgentMutationInfo): AgentRegistryInfo {
  return {
    ...registry,
    agents: registry.agents.map((a) =>
      a.id === agentId
        ? {
            ...a,
            name: input.name,
            description: input.description,
            avatarColor: input.avatarColor,
            model: input.model,
            permissionMode: input.permissionMode,
          }
        : a,
    ),
  };
}

export const useAgents = create<AgentsState>((set, get) => ({
  registry: null,
  detail: null,
  models: [],
  loaded: false,
  loading: false,
  saving: false,

  load: async (agentId) => {
    set({ loading: true });
    try {
      // Independent fetches must not waterfall: registry, models, and the
      // optional focused detail all go out in parallel.
      const [reg, models, detail] = await Promise.all([
        commands.listAgents(LOCAL_RUNNER),
        commands.listSelectableModels(LOCAL_RUNNER),
        agentId ? commands.getAgent(LOCAL_RUNNER, agentId) : Promise.resolve(null),
      ]);
      if (reg.status === "ok") set({ registry: reg.data, loaded: true });
      else toast.error(`Couldn't load agents: ${reg.error.message}`);
      if (models.status === "ok") set({ models: models.data });
      else toast.error(`Couldn't load models: ${models.error.message}`);
      if (detail) {
        if (detail.status === "ok") set({ detail: detail.data });
        else toast.error(`Couldn't load agent: ${detail.error.message}`);
      }
    } finally {
      set({ loading: false });
    }
  },

  loadDetail: async (agentId) => {
    const res = await commands.getAgent(LOCAL_RUNNER, agentId);
    if (res.status === "ok") set({ detail: res.data });
    else toast.error(`Couldn't load agent: ${res.error.message}`);
  },

  create: async (input) => {
    set({ saving: true });
    try {
      const res = await commands.createAgent(LOCAL_RUNNER, input);
      if (res.status === "error") {
        toast.error(`Create agent failed: ${res.error.message}`);
        return null;
      }
      const reg = get().registry;
      set({
        detail: res.data,
        registry: reg ? { ...reg, agents: [...reg.agents, res.data.summary] } : reg,
      });
      return res.data;
    } finally {
      set({ saving: false });
    }
  },

  update: async (agentId, input) => {
    // Snapshot, paint the representable fields optimistically, then commit
    // the server's authoritative detail — or restore the snapshot on error.
    const prev = { registry: get().registry, detail: get().detail };
    const optimistic: AgentDetailInfo | null =
      prev.detail && prev.detail.summary.id === agentId
        ? {
            ...prev.detail,
            summary: {
              ...prev.detail.summary,
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
            maxTurns: input.maxTurns,
            maxToolRounds: input.maxToolRounds,
          }
        : prev.detail;
    // Roster patch: use the optimistic detail's summary only when it really is
    // the target agent's detail — a focused detail for a *different* agent must
    // never be painted into the target row. Otherwise patch just the
    // representable summary fields from the mutation input.
    set({
      saving: true,
      detail: optimistic,
      registry: prev.registry
        ? optimistic && optimistic.summary.id === agentId
          ? patchRoster(prev.registry, agentId, optimistic)
          : patchRosterFields(prev.registry, agentId, input)
        : prev.registry,
    });
    try {
      const res = await commands.updateAgent(LOCAL_RUNNER, agentId, input);
      if (res.status === "error") {
        set({ registry: prev.registry, detail: prev.detail });
        toast.error(`Update agent failed: ${res.error.message}`);
        return false;
      }
      const reg = get().registry;
      set({
        detail: get().detail?.summary.id === agentId ? res.data : get().detail,
        registry: reg ? patchRoster(reg, agentId, res.data) : reg,
      });
      return true;
    } finally {
      set({ saving: false });
    }
  },

  duplicate: async (agentId) => {
    set({ saving: true });
    try {
      const res = await commands.duplicateAgent(LOCAL_RUNNER, agentId);
      if (res.status === "error") {
        toast.error(`Duplicate agent failed: ${res.error.message}`);
        return null;
      }
      const reg = get().registry;
      set({ registry: reg ? { ...reg, agents: [...reg.agents, res.data.summary] } : reg });
      return res.data;
    } finally {
      set({ saving: false });
    }
  },

  remove: async (agentId) => {
    // Not optimistic: deletion can be refused (last main agent), so the row
    // stays visible until the engine confirms and returns the new roster.
    set({ saving: true });
    try {
      const res = await commands.deleteAgent(LOCAL_RUNNER, agentId);
      if (res.status === "error") {
        toast.error(`Delete agent failed: ${res.error.message}`);
        return false;
      }
      set((s) => ({
        registry: res.data,
        detail: s.detail?.summary.id === agentId ? null : s.detail,
      }));
      return true;
    } finally {
      set({ saving: false });
    }
  },

  setDefault: async (agentId) => {
    const prev = get().registry;
    set({
      saving: true,
      registry: prev
        ? {
            ...prev,
            defaultAgentId: agentId,
            agents: prev.agents.map((a) => ({ ...a, isDefault: a.id === agentId })),
          }
        : prev,
    });
    try {
      const res = await commands.setDefaultAgent(LOCAL_RUNNER, agentId);
      if (res.status === "error") {
        set({ registry: prev });
        toast.error(`Default agent failed: ${res.error.message}`);
        return false;
      }
      set({ registry: res.data });
      return true;
    } finally {
      set({ saving: false });
    }
  },

  updateSubagentModel: async (model) => {
    const prev = get().registry;
    set({ saving: true, registry: prev ? { ...prev, subagentModel: model } : prev });
    try {
      const res = await commands.updateSubagentModel(LOCAL_RUNNER, model);
      if (res.status === "error") {
        set({ registry: prev });
        toast.error(`Subagent model failed: ${res.error.message}`);
        return false;
      }
      set({ registry: res.data });
      return true;
    } finally {
      set({ saving: false });
    }
  },
}));
