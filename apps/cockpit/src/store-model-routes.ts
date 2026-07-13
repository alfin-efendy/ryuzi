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
    const [routesResult, targetCapabilitiesResult] = await Promise.allSettled([
      commands.listModelRoutes("local"),
      commands.listModelRouteTargetCapabilities("local"),
    ]);
    set({ loaded: true });

    if (routesResult.status === "fulfilled") {
      if (routesResult.value.status === "ok") set({ routes: routesResult.value.data });
      else toast.error(`Routes failed: ${routesResult.value.error.message}`);
    } else {
      toast.error(`Routes failed: ${routesResult.reason instanceof Error ? routesResult.reason.message : String(routesResult.reason)}`);
    }

    if (targetCapabilitiesResult.status === "fulfilled") applyTargetCapabilities(set, targetCapabilitiesResult.value);
    else {
      toast.error(
        `Route target capabilities failed: ${
          targetCapabilitiesResult.reason instanceof Error
            ? targetCapabilitiesResult.reason.message
            : String(targetCapabilitiesResult.reason)
        }`,
      );
    }
  },
  save: async (route) => apply(set, await commands.saveModelRoute("local", route), "Save route"),
  remove: async (id) => apply(set, await commands.deleteModelRoute("local", id), "Delete route"),
}));
