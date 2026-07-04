import { create } from "zustand";
import { toast } from "sonner";
import { commands, type RuntimeInfo, type Result, type CmdError } from "./bindings";

// Runtimes domain store: catalog + persisted config + live detection snapshots
// from the engine. `hydrate` reads the fast persisted snapshot, then re-probes
// binaries and npm in the background.

type RuntimePatch = Partial<Pick<RuntimeInfo, "enabled" | "model" | "permMode" | "flags">>;

/** How many recent npm output lines to keep per runtime while updating. */
const MAX_UPDATE_LOG_LINES = 200;

type RuntimesState = {
  runtimes: RuntimeInfo[];
  loaded: boolean;
  refreshing: boolean;
  updating: Record<string, boolean>;
  updateLog: Record<string, string[]>;
  hydrate: () => Promise<void>;
  refresh: () => Promise<void>;
  update: (id: string, patch: RuntimePatch) => Promise<void>;
  setTier: (id: string, tierId: string, value: string | null, combo?: boolean) => Promise<void>;
  setDefault: (id: string) => Promise<void>;
  /** Kick off a streamed npm update for `id`; progress arrives via CoreEvent. */
  beginUpdate: (id: string) => Promise<void>;
  /** Append a streamed npm output line, capped at MAX_UPDATE_LOG_LINES. */
  onUpdateLog: (id: string, line: string) => void;
  /** Finish a streamed update: clear the in-flight flag and toast the outcome. */
  onUpdateDone: (id: string, ok: boolean, message: string | null) => void;
};

function applyResult(set: (partial: Partial<RuntimesState>) => void, res: Result<RuntimeInfo[], CmdError>, action: string) {
  if (res.status === "ok") set({ runtimes: res.data });
  else toast.error(`${action} failed: ${res.error.message}`);
}

export const useRuntimes = create<RuntimesState>((set, get) => ({
  runtimes: [],
  loaded: false,
  refreshing: false,
  updating: {},
  updateLog: {},

  hydrate: async () => {
    const res = await commands.listRuntimes();
    if (res.status === "ok") set({ runtimes: res.data, loaded: true });
    // Background re-probe: binaries, versions, npm latest, local models.
    void get().refresh();
  },

  refresh: async () => {
    if (get().refreshing) return;
    set({ refreshing: true });
    try {
      const res = await commands.refreshRuntimes();
      if (res.status === "ok") set({ runtimes: res.data, loaded: true });
    } finally {
      set({ refreshing: false });
    }
  },

  update: async (id, patch) => {
    const current = get().runtimes.find((a) => a.id === id);
    if (!current) return;
    const next = { ...current, ...patch };
    // Optimistic paint; the command returns the authoritative list.
    set({ runtimes: get().runtimes.map((a) => (a.id === id ? next : a)) });
    applyResult(set, await commands.updateRuntimeConfig(id, next.enabled, next.model || null, next.permMode, next.flags), "Runtime update");
  },

  setTier: async (id, tierId, value, combo = false) => {
    applyResult(set, await commands.setRuntimeTier(id, tierId, value, combo), "Tier update");
  },

  setDefault: async (id) => {
    set({ runtimes: get().runtimes.map((a) => ({ ...a, isDefault: a.id === id })) });
    applyResult(set, await commands.setDefaultRuntime(id), "Default runtime");
  },

  beginUpdate: async (id) => {
    set((st) => ({
      updating: { ...st.updating, [id]: true },
      updateLog: { ...st.updateLog, [id]: [] },
    }));
    const res = await commands.updateRuntime(id);
    if (res.status === "error") {
      set((st) => ({ updating: { ...st.updating, [id]: false } }));
      toast.error(`Runtime update failed: ${res.error.message}`);
    }
  },

  onUpdateLog: (id, line) =>
    set((st) => {
      const next = [...(st.updateLog[id] ?? []), line].slice(-MAX_UPDATE_LOG_LINES);
      return { updateLog: { ...st.updateLog, [id]: next } };
    }),

  onUpdateDone: (id, ok, message) => {
    set((st) => ({ updating: { ...st.updating, [id]: false } }));
    const agent = get().runtimes.find((a) => a.id === id);
    const label = agent?.name ?? id;
    if (ok) toast.success(`${label} updated`);
    else toast.error(`${label} update failed${message ? `: ${message}` : ""}`);
    void get().refresh();
  },
}));

/** The runtime marked default, falling back to the first runnable entry. */
export function defaultRuntimeOf(runtimes: RuntimeInfo[]): RuntimeInfo | undefined {
  return runtimes.find((a) => a.isDefault) ?? runtimes.find((a) => a.runnable);
}

export function runtimeById(runtimes: RuntimeInfo[], id: string): RuntimeInfo | undefined {
  return runtimes.find((a) => a.id === id);
}
