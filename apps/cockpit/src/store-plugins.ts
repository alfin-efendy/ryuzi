import { create } from "zustand";
import { toast } from "sonner";
import {
  commands,
  type CatalogStatus,
  type ComponentBootstrapStatus,
  type ComponentReleaseDetail,
  type DoctorFinding,
  type PluginInfo,
} from "./bindings";

// Plugins domain store. Definitions (manifests) live in the engine — builtin,
// component bundle, or user-authored — and this store mirrors the flattened
// `PluginInfo` list Cockpit needs for the Plugins hub screens.

/** Ids of every component-sourced plugin in the engine's list.
 *
 *  Replaces the old hardcoded `FIRST_PARTY_BUNDLE_IDS`. Component bundles are
 *  now registered as manifest-only `CorePlugin`s
 *  (`plugins::component_catalog`, `PluginSource::Component`), so they DO appear
 *  in `listPlugins` and the engine is the single source of truth for which
 *  components exist — Cockpit no longer has to mirror a Rust constant to know
 *  which ids to ask `pluginReleaseDetail` about. */
export function componentPluginIds(plugins: Pick<PluginInfo, "id" | "source">[]): string[] {
  return plugins.filter((p) => p.source === "component").map((p) => p.id);
}

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
  /** `component_bootstrap_status` snapshot for the Plugins view's retryable
   *  bootstrap banner — `null` until the first fetch resolves. */
  componentBootstrapStatus: ComponentBootstrapStatus | null;
  /** `plugin_release_detail` for every component-sourced id in `plugins`
   *  (see `componentPluginIds`) — the Installed tab's "Component plugins"
   *  section reads this for the release ledger (version, rollback), which a
   *  `PluginInfo` row does not carry. */
  componentPlugins: ComponentReleaseDetail[];
  componentPluginsLoaded: boolean;
  load: () => Promise<void>;
  loadDoctor: () => Promise<void>;
  refreshCatalog: () => Promise<void>;
  setEnabled: (id: string, on: boolean) => Promise<void>;
  uninstall: (id: string) => Promise<boolean>;
  update: (id: string, force: boolean) => Promise<void>;
  pin: (id: string, pinned: boolean, reason?: string) => Promise<void>;
  loadComponentBootstrapStatus: () => Promise<void>;
  loadComponentPlugins: () => Promise<void>;
  pluginReleaseDetail: (id: string) => Promise<ComponentReleaseDetail | null>;
  installComponentPlugin: (id: string, version?: string) => Promise<ComponentReleaseDetail | null>;
  rollbackComponentPlugin: (id: string, fromVersion: string, toVersion: string) => Promise<ComponentReleaseDetail | null>;
  /** Manually retries the first-party bootstrap (which otherwise only runs
   *  automatically at daemon start): attempts `installComponentPlugin` for
   *  every known first-party id, then reloads the bootstrap status and the
   *  component-plugins list. Individual failures are tolerated — the
   *  refreshed `componentBootstrapStatus.pending` is the source of truth for
   *  whether the banner should still show. */
  retryComponentBootstrap: () => Promise<void>;
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
  componentBootstrapStatus: null,
  componentPlugins: [],
  componentPluginsLoaded: false,

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

  // ---------- Component-plugin (WASM bundle) release management — Task 12 ----------
  //
  // Thin actions over the Task 11a RPC family (mirrors `plugin_detail`'s own
  // shape: `commands.X("local", ...)` → toast on error → reload). Component
  // bundles have no `PluginInfo` row to optimistically paint, so these read
  // straight from the RPC result rather than the `plugins` list.

  loadComponentBootstrapStatus: async () => {
    const res = await commands.componentBootstrapStatus("local");
    if (res.status === "ok") set({ componentBootstrapStatus: res.data });
    // Best-effort, same reasoning as `catalogStatus`/`restartRequired` in
    // `load()`: a failure here just leaves the banner at its prior state.
  },

  loadComponentPlugins: async () => {
    const ids = componentPluginIds(get().plugins);
    const details = await Promise.all(ids.map((id) => commands.pluginReleaseDetail("local", id)));
    const componentPlugins = details.flatMap((res) => (res.status === "ok" ? [res.data] : []));
    set({ componentPlugins, componentPluginsLoaded: true });
  },

  pluginReleaseDetail: async (id) => {
    const res = await commands.pluginReleaseDetail("local", id);
    if (res.status === "error") {
      toast.error(`Couldn't load release detail: ${res.error.message}`);
      return null;
    }
    return res.data;
  },

  installComponentPlugin: async (id, version) => {
    const res = await commands.installComponentPlugin("local", id, version ?? null);
    if (res.status === "error") {
      toast.error(`Install failed: ${res.error.message}`);
      return null;
    }
    toast.success(res.data.activeVersion ? `${id} installed — v${res.data.activeVersion}` : `${id} installed`);
    await get().loadComponentPlugins();
    return res.data;
  },

  rollbackComponentPlugin: async (id, fromVersion, toVersion) => {
    const res = await commands.rollbackComponentPlugin("local", id, fromVersion, toVersion);
    if (res.status === "error") {
      toast.error(`Rollback failed: ${res.error.message}`);
      return null;
    }
    toast.success(`Rolled back ${id} to v${toVersion}`);
    await get().loadComponentPlugins();
    return res.data;
  },

  retryComponentBootstrap: async () => {
    const ids = componentPluginIds(get().plugins);
    await Promise.allSettled(ids.map((id) => commands.installComponentPlugin("local", id, null)));
    await get().loadComponentPlugins();
    const res = await commands.componentBootstrapStatus("local");
    if (res.status !== "ok") return;
    set({ componentBootstrapStatus: res.data });
    if (res.data.pending) toast.warning(res.data.message ?? "Component plugins still couldn't be installed");
    else toast.success("Component plugins installed");
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
