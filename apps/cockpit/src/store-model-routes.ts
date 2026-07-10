import { create } from "zustand";
import { toast } from "sonner";
import { commands, type CmdError, type ModelRouteInfo, type Result } from "./bindings";
import { useRuntimes } from "./store-runtimes";

type ModelRoutesState = {
  routes: ModelRouteInfo[];
  loaded: boolean;
  hydrate: () => Promise<void>;
  save: (route: ModelRouteInfo) => Promise<boolean>;
  remove: (id: string) => Promise<boolean>;
};

function apply(set: (patch: Partial<ModelRoutesState>) => void, res: Result<ModelRouteInfo[], CmdError>, action: string): boolean {
  if (res.status === "ok") {
    set({ routes: res.data, loaded: true });
    // Route aliases feed the native runtime's selectable models
    // (selectable_native_models on the Rust side), so refresh the runtime
    // list — fire-and-forget so saving never blocks on it.
    void useRuntimes.getState().reloadList();
    return true;
  }
  toast.error(`${action} failed: ${res.error.message}`);
  return false;
}

export const useModelRoutes = create<ModelRoutesState>((set) => ({
  routes: [],
  loaded: false,

  hydrate: async () => {
    const res = await commands.listModelRoutes();
    if (res.status === "ok") set({ routes: res.data, loaded: true });
    else {
      set({ loaded: true });
      toast.error(`Routes failed: ${res.error.message}`);
    }
  },
  save: async (route) => apply(set, await commands.saveModelRoute(route), "Save route"),
  remove: async (id) => apply(set, await commands.deleteModelRoute(id), "Delete route"),
}));
