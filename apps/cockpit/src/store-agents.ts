import { create } from "zustand";
import { toast } from "sonner";
import { commands, type AgentInfo, type Result, type CmdError } from "./bindings";

// Agents domain store: catalog + persisted config + live detection snapshots
// from the engine. `hydrate` reads the fast persisted snapshot, then re-probes
// binaries and npm in the background.

type AgentPatch = Partial<Pick<AgentInfo, "enabled" | "model" | "permMode" | "flags">>;

type AgentsState = {
  agents: AgentInfo[];
  loaded: boolean;
  refreshing: boolean;
  hydrate: () => Promise<void>;
  refresh: () => Promise<void>;
  update: (id: string, patch: AgentPatch) => Promise<void>;
  setTier: (id: string, tierId: string, value: string | null, combo?: boolean) => Promise<void>;
  setDefault: (id: string) => Promise<void>;
};

function applyResult(
  set: (partial: Partial<AgentsState>) => void,
  res: Result<AgentInfo[], CmdError>,
  action: string,
) {
  if (res.status === "ok") set({ agents: res.data });
  else toast.error(`${action} failed: ${res.error.message}`);
}

export const useAgents = create<AgentsState>((set, get) => ({
  agents: [],
  loaded: false,
  refreshing: false,

  hydrate: async () => {
    const res = await commands.listAgents();
    if (res.status === "ok") set({ agents: res.data, loaded: true });
    // Background re-probe: binaries, versions, npm latest, local models.
    void get().refresh();
  },

  refresh: async () => {
    if (get().refreshing) return;
    set({ refreshing: true });
    try {
      const res = await commands.refreshAgents();
      if (res.status === "ok") set({ agents: res.data, loaded: true });
    } finally {
      set({ refreshing: false });
    }
  },

  update: async (id, patch) => {
    const current = get().agents.find((a) => a.id === id);
    if (!current) return;
    const next = { ...current, ...patch };
    // Optimistic paint; the command returns the authoritative list.
    set({ agents: get().agents.map((a) => (a.id === id ? next : a)) });
    applyResult(
      set,
      await commands.updateAgent(id, next.enabled, next.model || null, next.permMode, next.flags),
      "Agent update",
    );
  },

  setTier: async (id, tierId, value, combo = false) => {
    applyResult(set, await commands.setAgentTier(id, tierId, value, combo), "Tier update");
  },

  setDefault: async (id) => {
    set({ agents: get().agents.map((a) => ({ ...a, isDefault: a.id === id })) });
    applyResult(set, await commands.setDefaultAgent(id), "Default agent");
  },
}));

/** The agent marked default, falling back to the first runnable entry. */
export function defaultAgentOf(agents: AgentInfo[]): AgentInfo | undefined {
  return agents.find((a) => a.isDefault) ?? agents.find((a) => a.runnable);
}

export function agentById(agents: AgentInfo[], id: string): AgentInfo | undefined {
  return agents.find((a) => a.id === id);
}
