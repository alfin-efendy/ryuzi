import { create } from "zustand";
import { toast } from "sonner";
import { commands } from "./bindings";

// Native agent settings store (replacement for the runtimes-domain store):
// the default model + permission mode persisted in the engine's settings KV,
// plus the selectable-model list the composer and Settings share.

// Not exported: nothing outside this module reads the stored permission mode
// directly anymore (the Settings UI control was dropped — see `permMode`
// below). Keep it local for typing `permMode`/the settings payload.
type AgentPermMode = "plan" | "ask" | "edit" | "full";

type AgentState = {
  /** Selectable models: enabled route aliases, then provider/model ids. */
  models: string[];
  /** Pinned default model; null = router default (first usable provider). */
  model: string | null;
  /**
   * Hydrated passthrough of the engine's stored permission mode. Retained
   * even though Settings no longer has a control for it: `setModel` sends
   * this value back unchanged on every write, and the backend's
   * `set_agent_settings` treats a `None` permMode as DELETE-the-key — so
   * dropping this field would silently erase the user's persisted
   * `agent_perm_mode` the next time they change the default model.
   */
  permMode: AgentPermMode | null;
  /**
   * True only after a successful `getAgentSettings` load. Guards `setModel`:
   * before hydration (or after a failed `load()`), `model`/`permMode` are
   * still null, and sending nulls to `set_agent_settings` deletes the
   * persisted keys instead of leaving them alone.
   */
  loaded: boolean;
  load: () => Promise<void>;
  setModel: (model: string | null) => Promise<void>;
};

export const useAgent = create<AgentState>((set, get) => ({
  models: [],
  model: null,
  permMode: null,
  loaded: false,

  load: async () => {
    const [settings, models] = await Promise.all([commands.getAgentSettings(), commands.listSelectableModels()]);
    if (settings.status === "ok") {
      set({ model: settings.data.model, permMode: (settings.data.permMode as AgentPermMode | null) ?? null, loaded: true });
    } else {
      toast.error(`Couldn't load agent settings: ${settings.error.message}`);
    }
    if (models.status === "ok") set({ models: models.data });
  },

  setModel: async (model) => {
    // Refuse to write until a successful load has hydrated model/permMode —
    // otherwise this would send nulls and delete the persisted settings.
    if (!get().loaded) return;
    const prev = get().model;
    // Optimistic paint; roll back on a rejected write.
    set({ model });
    const res = await commands.setAgentSettings(model, get().permMode);
    if (res.status === "error") {
      set({ model: prev });
      toast.error(`Default model failed: ${res.error.message}`);
    }
  },
}));
