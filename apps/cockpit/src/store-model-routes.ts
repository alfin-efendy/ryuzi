import { create } from "zustand";
import { toast } from "sonner";
import { commands, type CmdError, type ModelRouteInfo, type Result } from "./bindings";
import { useStore } from "./store";

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
    void useStore.getState().refreshModelConfiguration();
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
