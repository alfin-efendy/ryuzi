import { create } from "zustand";
import { toast } from "sonner";
import { commands, type PluginInfo } from "./bindings";

// Plugins domain store. Definitions (manifests) live in the engine — builtin,
// embedded catalog, or user-authored — and this store mirrors the flattened
// `PluginInfo` list Cockpit needs for the Plugins hub screens.

type PluginsState = {
  plugins: PluginInfo[];
  loaded: boolean;
  load: () => Promise<void>;
  setEnabled: (id: string, on: boolean) => Promise<void>;
  uninstall: (id: string) => Promise<boolean>;
};

export const usePlugins = create<PluginsState>((set, get) => ({
  plugins: [],
  loaded: false,

  load: async () => {
    const res = await commands.listPlugins();
    if (res.status === "ok") set({ plugins: res.data, loaded: true });
    else toast.error(`Plugin list failed: ${res.error.message}`);
  },

  setEnabled: async (id, on) => {
    // Optimistic paint so the toggle feels instant; `set_plugin_enabled`
    // returns no list (unlike most toggle commands), so reload afterwards
    // to reconcile with the engine's authoritative state either way.
    set({ plugins: get().plugins.map((p) => (p.id === id ? { ...p, enabled: on } : p)) });
    const res = await commands.setPluginEnabled(id, on);
    if (res.status === "error") toast.error(`Plugin update failed: ${res.error.message}`);
    await get().load();
  },

  uninstall: async (id) => {
    const res = await commands.uninstallPlugin(id);
    if (res.status === "error") {
      toast.error(`Uninstall failed: ${res.error.message}`);
      return false;
    }
    set({ plugins: res.data, loaded: true });
    return true;
  },
}));

export function pluginById(plugins: PluginInfo[], id: string): PluginInfo | undefined {
  return plugins.find((p) => p.id === id);
}

/** Browse tab: only entries not yet installed — installing removes the card. */
export function browsePlugins(plugins: PluginInfo[]): PluginInfo[] {
  return plugins.filter((p) => !p.installed);
}

/** Installed tab: providers, gateways, and skill packs that are set up.
 *  MCP apps render from `useApps` separately. */
export function installedPlugins(plugins: PluginInfo[]): PluginInfo[] {
  return plugins.filter((p) => p.installed);
}
