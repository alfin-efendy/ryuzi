import { create } from "zustand";
import { toast } from "sonner";
import { commands, type CmdError, type ModelRouteInfo, type ModelRouteTargetCapability, type Result } from "./bindings";
import { useStore } from "./store";

type ModelRoutesState = {
  routes: ModelRouteInfo[];
  targetCapabilities: ModelRouteTargetCapability[];
  targetCapabilitiesLoaded: boolean;
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
  if (res.status === "ok") set({ targetCapabilities: res.data, targetCapabilitiesLoaded: true });
  else toast.error(`Route target capabilities failed: ${res.error.message}`);
}

export const useModelRoutes = create<ModelRoutesState>((set) => ({
  routes: [],
  targetCapabilities: [],
  targetCapabilitiesLoaded: false,
  loaded: false,

  hydrate: async () => {
    const { loaded, targetCapabilitiesLoaded } = useModelRoutes.getState();
    const shouldLoadRoutes = !loaded;
    const shouldLoadTargetCapabilities = !loaded || !targetCapabilitiesLoaded;
    const [routesResult, targetCapabilitiesResult] = await Promise.allSettled([
      shouldLoadRoutes ? commands.listModelRoutes("local") : Promise.resolve(null),
      shouldLoadTargetCapabilities ? commands.listModelRouteTargetCapabilities("local") : Promise.resolve(null),
    ]);

    if (routesResult.status === "fulfilled" && routesResult.value !== null) {
      if (routesResult.value.status === "ok") set({ routes: routesResult.value.data, loaded: true });
      else toast.error(`Routes failed: ${routesResult.value.error.message}`);
    } else if (routesResult.status === "rejected") {
      toast.error(`Routes failed: ${routesResult.reason instanceof Error ? routesResult.reason.message : String(routesResult.reason)}`);
    }

    if (targetCapabilitiesResult.status === "fulfilled" && targetCapabilitiesResult.value !== null) {
      applyTargetCapabilities(set, targetCapabilitiesResult.value);
    } else if (targetCapabilitiesResult.status === "rejected") {
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
