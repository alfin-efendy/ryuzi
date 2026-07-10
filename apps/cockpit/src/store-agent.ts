import { create } from "zustand";
import { toast } from "sonner";
import { commands } from "./bindings";

// Native agent settings store (replacement for the runtimes-domain store):
// the default model + permission mode persisted in the engine's settings KV,
// plus the selectable-model list the composer and Settings share.

export type AgentPermMode = "plan" | "ask" | "edit" | "full";

type AgentState = {
  /** Selectable models: enabled route aliases, then provider/model ids. */
  models: string[];
  /** Pinned default model; null = router default (first usable provider). */
  model: string | null;
  /** null until loaded; the engine default is "ask". */
  permMode: AgentPermMode | null;
  load: () => Promise<void>;
  setModel: (model: string | null) => Promise<void>;
  setPermMode: (mode: AgentPermMode) => Promise<void>;
};

export const useAgent = create<AgentState>((set, get) => ({
  models: [],
  model: null,
  permMode: null,

  load: async () => {
    const [settings, models] = await Promise.all([commands.getAgentSettings(), commands.listSelectableModels()]);
    if (settings.status === "ok") {
      set({ model: settings.data.model, permMode: (settings.data.permMode as AgentPermMode | null) ?? null });
    }
    if (models.status === "ok") set({ models: models.data });
  },

  setModel: async (model) => {
    const prev = get().model;
    // Optimistic paint; roll back on a rejected write.
    set({ model });
    const res = await commands.setAgentSettings(model, get().permMode);
    if (res.status === "error") {
      set({ model: prev });
      toast.error(`Default model failed: ${res.error.message}`);
    }
  },

  setPermMode: async (permMode) => {
    const prev = get().permMode;
    set({ permMode });
    const res = await commands.setAgentSettings(get().model, permMode);
    if (res.status === "error") {
      set({ permMode: prev });
      toast.error(`Permission mode failed: ${res.error.message}`);
    }
  },
}));
