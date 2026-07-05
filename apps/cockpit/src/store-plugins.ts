import { create } from "zustand";
import { toast } from "sonner";
import { commands, type PluginInfo } from "./bindings";

// Plugins domain store. Definitions (manifests) live in the engine — builtin,
// embedded catalog, or user-authored — and this store mirrors the flattened
// `PluginInfo` list Cockpit needs for the catalog and sidebar screens.

type PluginsState = {
  plugins: PluginInfo[];
  loaded: boolean;
  load: () => Promise<void>;
  setEnabled: (id: string, on: boolean) => Promise<void>;
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
}));

export function pluginById(plugins: PluginInfo[], id: string): PluginInfo | undefined {
  return plugins.find((p) => p.id === id);
}

/**
 * Plugins the sidebar shows as their own menu row: enabled integrations
 * (catalog or user-authored). Core builtins (native, claude-code, discord,
 * providers) never get a row here — they're wired into the fixed NAV items
 * above, and `PluginInfo` itself carries no menu-contribution field (that
 * lives on `PluginDetail.menuLabel`, only available per-plugin via
 * `pluginDetail`, not in the bulk `listPlugins` response the sidebar uses).
 */
export function sidebarPlugins(plugins: PluginInfo[]): PluginInfo[] {
  return plugins.filter((p) => p.enabled && (p.source === "catalog" || p.source === "user"));
}
