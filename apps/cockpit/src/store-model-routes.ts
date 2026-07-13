import { create } from "zustand";
import { toast } from "sonner";
import { commands, type CmdError, type ModelRouteInfo, type ModelRouteTargetCapability, type Result } from "./bindings";
import { useStore } from "./store";

type ModelRoutesState = {
  routes: ModelRouteInfo[];
  targetCapabilities: ModelRouteTargetCapability[];
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

function applyTargetCapabilities(
  set: (patch: Partial<ModelRoutesState>) => void,
  res: Result<ModelRouteTargetCapability[], CmdError>,
): void {
  if (res.status === "ok") set({ targetCapabilities: res.data });
  else toast.error(`Route target capabilities failed: ${res.error.message}`);
}

export const useModelRoutes = create<ModelRoutesState>((set) => ({
  routes: [],
  targetCapabilities: [],
  loaded: false,

  hydrate: async () => {
    const [routes, targetCapabilities] = await Promise.all([
      commands.listModelRoutes("local"),
      commands.listModelRouteTargetCapabilities("local"),
    ]);
    if (routes.status === "ok") set({ routes: routes.data, loaded: true });
    else {
      set({ loaded: true });
      toast.error(`Routes failed: ${routes.error.message}`);
    }
    applyTargetCapabilities(set, targetCapabilities);
  },
  save: async (route) => apply(set, await commands.saveModelRoute("local", route), "Save route"),
  remove: async (id) => apply(set, await commands.deleteModelRoute("local", id), "Delete route"),
}));
