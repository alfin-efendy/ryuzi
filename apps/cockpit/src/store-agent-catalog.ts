import { create } from "zustand";
import { commands, type AgentConfigurationCatalogInfo } from "./bindings";
import { LOCAL_RUNNER } from "./lib/session-key";

type AgentConfigurationCatalogState = {
  catalog: AgentConfigurationCatalogInfo | null;
  loading: boolean;
  error: string | null;
  load: () => Promise<void>;
};

function errorMessage(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

export const useAgentConfigurationCatalog = create<AgentConfigurationCatalogState>((set, get) => {
  let inFlight: Promise<void> | null = null;

  return {
    catalog: null,
    loading: false,
    error: null,
    load: () => {
      if (get().catalog !== null) return Promise.resolve();
      if (inFlight !== null) return inFlight;
      set({ loading: true, error: null });
      inFlight = (async () => {
        try {
          const result = await commands.getAgentConfigurationCatalog(LOCAL_RUNNER);
          if (result.status === "ok") set({ catalog: result.data, loading: false, error: null });
          else set({ loading: false, error: result.error.message });
        } catch (error) {
          set({ loading: false, error: errorMessage(error) });
        } finally {
          inFlight = null;
        }
      })();
      return inFlight;
    },
  };
});
