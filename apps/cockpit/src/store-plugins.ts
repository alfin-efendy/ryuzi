import { create } from "zustand";
import { toast } from "sonner";
import { commands, type CatalogStatus, type DoctorFinding, type PluginInfo } from "./bindings";

// Plugins domain store. Definitions (manifests) live in the engine — builtin,
// embedded catalog, or user-authored — and this store mirrors the flattened
// `PluginInfo` list Cockpit needs for the Plugins hub screens.

type PluginsState = {
  plugins: PluginInfo[];
  loaded: boolean;
  /** Whether an install/update since app start needs a restart to apply. */
  restartRequired: boolean;
  /** `plugin_doctor` findings, cached so the Installed grid, the plugin
   *  detail attach-failure banner, and `DoctorPanel` all read the same
   *  snapshot instead of triggering their own redundant fetches. */
  doctorFindings: DoctorFinding[];
  doctorLoaded: boolean;
  /** Last accepted remote-catalog feed snapshot — `null` until the first
   *  `catalog_status`/`refresh_catalog` call resolves. */
  catalogStatus: CatalogStatus | null;
  load: () => Promise<void>;
  loadDoctor: () => Promise<void>;
  refreshCatalog: () => Promise<void>;
  setEnabled: (id: string, on: boolean) => Promise<void>;
  uninstall: (id: string) => Promise<boolean>;
  update: (id: string, force: boolean) => Promise<void>;
  pin: (id: string, pinned: boolean, reason?: string) => Promise<void>;
};

/** `update_all_plugins`'s outcome summary, shared so the store action and any
 *  ad hoc caller (Update-all button) report the same counts. */
export function summarizeUpdateAll(entries: { id: string; outcome: { kind: string; detail?: unknown } }[]): string {
  const updated = entries.filter((e) => e.outcome.kind === "updated").length;
  const failed = entries.filter((e) => e.outcome.kind === "failed").length;
  const needsReack = entries.filter((e) => e.outcome.kind === "needsReack").length;
  const parts = [`${updated} updated`];
  if (needsReack > 0) parts.push(`${needsReack} need re-review`);
  if (failed > 0) parts.push(`${failed} failed`);
  return parts.join(", ");
}

export const usePlugins = create<PluginsState>((set, get) => ({
  plugins: [],
  loaded: false,
  restartRequired: false,
  doctorFindings: [],
  doctorLoaded: false,
  catalogStatus: null,

  load: async () => {
    const res = await commands.listPlugins("local");
    if (res.status === "ok") set({ plugins: res.data, loaded: true });
    else toast.error(`Plugin list failed: ${res.error.message}`);

    // Best-effort: a failure here shouldn't block the plugin list itself, so
    // it's silently skipped (the restart banner just stays as it was).
    const restartRes = await commands.pluginsRestartRequired("local");
    if (restartRes.status === "ok") set({ restartRequired: restartRes.data });

    // Best-effort, same reasoning: the Browse tab's status line just stays
    // stale (or empty) rather than blocking the plugin list on it.
    const catalogRes = await commands.catalogStatus("local");
    if (catalogRes.status === "ok") set({ catalogStatus: catalogRes.data });
  },

  loadDoctor: async () => {
    const res = await commands.pluginDoctor("local");
    if (res.status === "ok") set({ doctorFindings: res.data, doctorLoaded: true });
    else toast.error(`Doctor check failed: ${res.error.message}`);
  },

  refreshCatalog: async () => {
    const res = await commands.refreshCatalog("local");
    if (res.status === "error") {
      toast.error(`Catalog refresh failed: ${res.error.message}`);
      return;
    }
    set({ catalogStatus: res.data });
    if (res.data.outcome === "ok") {
      const blockedPart = res.data.blocked > 0 ? `, ${res.data.blocked} blocked` : "";
      toast.success(`Catalog refreshed — ${res.data.entries} ${res.data.entries === 1 ? "entry" : "entries"}${blockedPart}`);
    } else {
      toast.warning("Catalog refresh did not apply — no verified feed available yet");
    }
    await get().load();
  },

  setEnabled: async (id, on) => {
    // Optimistic paint so the toggle feels instant; `set_plugin_enabled`
    // returns no list (unlike most toggle commands), so reload afterwards
    // to reconcile with the engine's authoritative state either way.
    set({ plugins: get().plugins.map((p) => (p.id === id ? { ...p, enabled: on } : p)) });
    const res = await commands.setPluginEnabled("local", id, on);
    if (res.status === "error") toast.error(`Plugin update failed: ${res.error.message}`);
    await get().load();
  },

  uninstall: async (id) => {
    const res = await commands.uninstallPlugin("local", id);
    if (res.status === "error") {
      toast.error(`Uninstall failed: ${res.error.message}`);
      return false;
    }
    set({ plugins: res.data, loaded: true });
    return true;
  },

  update: async (id, force) => {
    const res = await commands.updatePlugin("local", id, force);
    if (res.status === "error") {
      toast.error(`Update failed: ${res.error.message}`);
    } else {
      switch (res.data.kind) {
        case "updated":
          toast.success("Plugin updated");
          break;
        case "alreadyCurrent":
          toast.info("Already up to date");
          break;
        case "skippedPinned":
          toast.info("Skipped — plugin is pinned");
          break;
        case "localEdits":
          toast.warning("Skipped — local edits detected on disk");
          break;
        case "failed":
          toast.error(`Update failed: ${res.data.detail}`);
          break;
        case "needsReack":
          toast.warning("This update adds hook scripts — reinstall the source to review and accept them.");
          break;
      }
    }
    await get().load();
    if (get().doctorLoaded) await get().loadDoctor();
  },

  pin: async (id, pinned, reason) => {
    // Optimistic paint (mirrors `setEnabled`) so the toggle feels instant;
    // the reload below reconciles with the engine's persisted
    // `plugin_installs.pinned` ledger flag either way — success or error —
    // so a failed write never leaves a stale optimistic pin behind.
    set({ plugins: get().plugins.map((p) => (p.id === id ? { ...p, pinned } : p)) });
    const res = await commands.setPluginPin("local", id, pinned, reason ?? null);
    if (res.status === "error") toast.error(`Pin update failed: ${res.error.message}`);
    else toast.success(pinned ? "Pinned" : "Unpinned");
    await get().load();
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
