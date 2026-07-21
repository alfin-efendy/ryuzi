import { useEffect, useState } from "react";
import { Blocks, CircleAlert, MonitorUp, Pin, PinOff, Plus, RefreshCw, Sparkles, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { Badge, Button, Combobox, Segmented, SettingsCard as Card } from "@ryuzi/ui";
import {
  commands,
  type AppInfo,
  type CatalogStatus,
  type ComponentBootstrapStatus,
  type ComponentReleaseDetail,
  type PluginInfo,
} from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";
import { BlockedBadge, Chip, IconChip, Pill, PluginStatusBadge, StatusDot } from "@/components/common/bits";
import { DoctorPanel } from "@/components/DoctorPanel";
import { useApps } from "@/store-apps";
import { useGateways } from "@/store-gateways";
import { browsePlugins, installedPlugins, summarizeUpdateAll, usePlugins } from "@/store-plugins";
import { useSkills } from "@/store-skills";
import { pluginIcon } from "@/lib/plugin-icons";
import { AddAppModal } from "@/components/modals/AddAppModal";
import { AddConnectionModal } from "@/components/modals/AddConnectionModal";
import { InstallWizardModal } from "@/components/modals/InstallWizardModal";
import { SkillInstallModal } from "@/components/modals/SkillInstallModal";
import { useNav } from "@/store-nav";
import { useConnections } from "@/store-connections";

const WARN = "#F59E0B";

type PluginsTab = "installed" | "browse";

function appStatus(app: AppInfo): { color: string; label: string } {
  if (app.status === "connected") return { color: "#22C55E", label: "Connected" };
  if (app.status === "error") return { color: "#EF4444", label: "Error" };
  return { color: "var(--muted-foreground)", label: "Unchecked" };
}

/** Pure so Browse's category filter stays unit-testable without mounting
 *  the view — `"all"` (the default) passes every plugin through untouched. */
export function filterByCategory(plugins: PluginInfo[], category: string): PluginInfo[] {
  if (category === "all") return plugins;
  return plugins.filter((p) => p.categories.includes(category));
}

/** Subtle Browse-tab status line summarizing the last `catalog_status`/
 *  `refresh_catalog` snapshot. Pure (and exported) so it stays unit-testable
 *  without mounting the view. */
export function catalogStatusLabel(status: CatalogStatus): string {
  if (!status.lastFetchAt) return "Catalog not yet fetched";
  const when = new Date(status.lastFetchAt).toLocaleString();
  const blockedPart = status.blocked > 0 ? `, ${status.blocked} blocked` : "";
  return `Catalog seq ${status.sequence} · ${status.entries} entries${blockedPart} · fetched ${when}`;
}

// ---------- Component-plugin (WASM bundle) release management — Task 12 ----------

/** The retryable bootstrap banner's message, or `null` when there's nothing
 *  to show (not yet loaded, or the last automatic attempt at daemon start
 *  fully completed). Pure and exported so it stays unit-testable without
 *  mounting the view. */
export function bootstrapBannerMessage(status: ComponentBootstrapStatus | null): string | null {
  if (!status?.pending) return null;
  return status.message ?? "Some first-party component plugins couldn't be installed automatically.";
}

/** One-line status for a component (WASM bundle) plugin's release ledger —
 *  drives the "Component plugins" section's summary line. Pure and exported
 *  so it stays unit-testable without mounting the view. */
export function componentPluginStatusLabel(detail: ComponentReleaseDetail): string {
  return detail.activeVersion ? `v${detail.activeVersion} active` : "Not installed";
}

function BrowseCard({ plugin, onInstall }: { plugin: PluginInfo; onInstall: () => void }) {
  const Icon = pluginIcon(plugin.icon);
  const blocked = plugin.blockedReason !== null;
  return (
    <Card className="flex flex-col gap-3 px-[18px] py-4">
      <div className="flex items-start gap-3">
        <IconChip icon={Icon} size={38} />
        <span className="min-w-0 flex-1">
          <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-sm font-semibold">{plugin.name}</span>
          <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-[11.5px] text-muted-foreground">
            {plugin.description}
          </span>
        </span>
        {blocked ? <BlockedBadge /> : <PluginStatusBadge verified={plugin.verified} experimental={plugin.experimental} />}
      </div>
      <div className="flex flex-wrap gap-1.5">
        {plugin.categories.map((c) => (
          <Badge key={c} variant="outline">
            {c}
          </Badge>
        ))}
      </div>
      {blocked && plugin.blockedReason && <p className="m-0 text-[11.5px] text-destructive">{plugin.blockedReason}</p>}
      <div className="flex items-center gap-2 pt-0.5">
        <span className="flex-1" />
        {!blocked && (
          <Button size="sm" onClick={onInstall} aria-label={`Install ${plugin.name}`}>
            Install
          </Button>
        )}
      </div>
    </Card>
  );
}

function InstalledPluginCard({
  plugin,
  onOpen,
  onUninstall,
  onUpdate,
  onTogglePin,
  pinned,
  attachFailed,
  updating,
}: {
  plugin: PluginInfo;
  onOpen: (() => void) | null;
  onUninstall: () => void;
  onUpdate: () => void;
  onTogglePin: () => void;
  pinned: boolean;
  attachFailed: boolean;
  updating: boolean;
}) {
  const Icon = pluginIcon(plugin.icon);
  const isSkillPack = plugin.kind === "skill-pack";
  const blocked = plugin.blockedReason !== null;
  return (
    <Card className="flex flex-col gap-3 px-[18px] py-4">
      <div className="flex items-start gap-3">
        <IconChip icon={Icon} size={38} />
        <span className="min-w-0 flex-1">
          <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-sm font-semibold">{plugin.name}</span>
          <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-[11.5px] text-muted-foreground">
            {plugin.description}
          </span>
        </span>
        <Badge variant="outline">{plugin.kind}</Badge>
      </div>
      {(pinned || attachFailed || blocked) && (
        <div className="flex flex-wrap items-center gap-1.5">
          {pinned && (
            <Pill variant="mono">
              <Pin aria-hidden size={9} strokeWidth={2} className="mr-1 inline align-[-1px]" />
              Pinned
            </Pill>
          )}
          {attachFailed && <Pill variant="warn">Attach failed</Pill>}
          {blocked && <BlockedBadge />}
        </div>
      )}
      <div className="flex items-center gap-2 pt-0.5">
        <span className="flex-1" />
        {onOpen && (
          <Button variant="outline" size="sm" onClick={onOpen} aria-label={`Open ${plugin.name}`}>
            Open
          </Button>
        )}
        {isSkillPack && (
          <>
            <Button variant="outline" size="sm" onClick={onUpdate} disabled={updating} aria-label={`Update ${plugin.name}`}>
              <RefreshCw aria-hidden size={13} strokeWidth={2} className={updating ? "animate-spin" : undefined} />
              {updating ? "Updating…" : "Update"}
            </Button>
            <Button variant="outline" size="sm" onClick={onTogglePin} aria-label={`${pinned ? "Unpin" : "Pin"} ${plugin.name}`}>
              {pinned ? <PinOff aria-hidden size={13} strokeWidth={2} /> : <Pin aria-hidden size={13} strokeWidth={2} />}
              {pinned ? "Unpin" : "Pin"}
            </Button>
          </>
        )}
        <Button variant="outline" size="sm" onClick={onUninstall} aria-label={`Uninstall ${plugin.name}`}>
          <Trash2 aria-hidden size={13} strokeWidth={2} />
          Uninstall
        </Button>
      </div>
    </Card>
  );
}

export function PluginsView() {
  const nav = useNav();
  const { apps, loaded, hydrate } = useApps();
  const gateways = useGateways((s) => s.gateways);
  const {
    plugins,
    loaded: pluginsLoaded,
    load: loadPlugins,
    uninstall,
    update: updatePlugin,
    pin: pinPlugin,
    doctorFindings,
    doctorLoaded,
    loadDoctor,
    catalogStatus,
    refreshCatalog,
    componentBootstrapStatus,
    componentPlugins,
    componentPluginsLoaded,
    loadComponentBootstrapStatus,
    loadComponentPlugins,
    retryComponentBootstrap,
  } = usePlugins();
  const { installProvider, uninstallProvider } = useConnections();
  const skills = useSkills((s) => s.skills);
  const skillsLoading = useSkills((s) => s.loading);
  const refreshSkills = useSkills((s) => s.refresh);
  const refreshSkillPack = useSkills((s) => s.refreshSkillPack);
  const removeSkillPack = useSkills((s) => s.remove);
  const [tab, setTab] = useState<PluginsTab>("installed");
  const [category, setCategory] = useState("all");
  const [addAppOpen, setAddAppOpen] = useState(false);
  const [skillInstall, setSkillInstall] = useState<{ initialSource?: string } | null>(null);
  const [installingPlugin, setInstallingPlugin] = useState<PluginInfo | null>(null);
  const [connectingFamily, setConnectingFamily] = useState<string | null>(null);
  const [updatingIds, setUpdatingIds] = useState<Set<string>>(new Set());
  const [updatingAll, setUpdatingAll] = useState(false);
  const [doctorOpen, setDoctorOpen] = useState(false);
  const [refreshingCatalog, setRefreshingCatalog] = useState(false);
  const [retryingBootstrap, setRetryingBootstrap] = useState(false);

  useEffect(() => {
    void hydrate();
  }, [hydrate]);

  useEffect(() => {
    if (!pluginsLoaded) void loadPlugins();
  }, [pluginsLoaded, loadPlugins]);

  // Component (WASM bundle) plugins — e.g. mimo/opencode — are never
  // `CorePlugin`s, so they never appear in `listPlugins`; this is Cockpit's
  // only fetch for their release ledger + bootstrap status.
  useEffect(() => {
    void loadComponentBootstrapStatus();
  }, [loadComponentBootstrapStatus]);

  useEffect(() => {
    if (!componentPluginsLoaded) void loadComponentPlugins();
  }, [componentPluginsLoaded, loadComponentPlugins]);

  useEffect(() => {
    if (!doctorLoaded) void loadDoctor();
  }, [doctorLoaded, loadDoctor]);

  useEffect(() => {
    void refreshSkills();
  }, [refreshSkills]);

  const browse = filterByCategory(browsePlugins(plugins), category);
  const categories = Array.from(new Set(browsePlugins(plugins).flatMap((p) => p.categories))).sort();
  const installed = installedPlugins(plugins);
  const installedSkillPacks = installed.filter((p) => p.kind === "skill-pack");
  const attachFailedIds = new Set(doctorFindings.filter((f) => f.kind === "attach-failed").map((f) => f.pluginId));
  const issueCount = doctorFindings.length;

  const installBusy = installingPlugin !== null || connectingFamily !== null || skillInstall !== null;

  const startInstall = (plugin: PluginInfo) => {
    if (installBusy) return;
    if (plugin.kind === "provider") {
      // Providers install into the persisted set (visibility only); adding an
      // account is a separate step from the provider's detail view. Re-fetch
      // the plugins list so the card moves from Browse to Installed.
      void installProvider(plugin.family ?? plugin.id).then((ok) => ok && void loadPlugins());
    } else if (plugin.kind === "skill-pack") {
      // Curated catalog packs resolve `completed: true` immediately; the
      // trust step only ever shows up for arbitrary sources (see
      // `SkillInstallModal`'s doc comment) — same two-phase gate either way.
      setSkillInstall({ initialSource: plugin.id });
    } else {
      setInstallingPlugin(plugin);
    }
  };

  const openInstalled = (plugin: PluginInfo): (() => void) | null => {
    if (plugin.kind === "provider") {
      const family = plugin.family ?? plugin.id;
      return () => nav.navigate({ kind: "providerDetail", provider: family });
    }
    if (plugin.kind === "skill-pack") return null;
    return () => nav.navigate({ kind: "pluginDetail", id: plugin.id });
  };

  const uninstallPlugin = (plugin: PluginInfo) => {
    if (plugin.kind === "provider") {
      // Uninstalling a provider only removes it from the installed set; its
      // connections stay intact. Re-fetch so the card returns to Browse.
      void uninstallProvider(plugin.family ?? plugin.id).then((ok) => ok && void loadPlugins());
      return;
    }
    // `uninstall` already swaps in the fresh plugins list from the command,
    // so the Installed/Browse grids reconcile on their own. The extra
    // `refreshSkills` is only to update the manual "Skill sources" card,
    // whose rows come from the skills store, not the plugins list.
    void uninstall(plugin.id).then((ok) => {
      if (ok) void refreshSkills();
    });
  };

  const runUpdate = async (plugin: PluginInfo) => {
    if (updatingIds.has(plugin.id)) return;
    setUpdatingIds((s) => new Set(s).add(plugin.id));
    await updatePlugin(plugin.id, false);
    setUpdatingIds((s) => {
      const next = new Set(s);
      next.delete(plugin.id);
      return next;
    });
  };

  const runTogglePin = (plugin: PluginInfo) => {
    void pinPlugin(plugin.id, !plugin.pinned, plugin.pinned ? undefined : "Pinned from Cockpit");
  };

  const runUpdateAll = async () => {
    if (updatingAll) return;
    setUpdatingAll(true);
    const res = await commands.updateAllPlugins(LOCAL_RUNNER);
    setUpdatingAll(false);
    if (res.status === "error") {
      toast.error(`Update all failed: ${res.error.message}`);
      return;
    }
    toast.success(`Update all — ${summarizeUpdateAll(res.data)}`);
    await loadPlugins();
    if (doctorLoaded) void loadDoctor();
  };

  const runRefreshCatalog = async () => {
    if (refreshingCatalog) return;
    setRefreshingCatalog(true);
    await refreshCatalog();
    setRefreshingCatalog(false);
  };

  const runRetryBootstrap = async () => {
    if (retryingBootstrap) return;
    setRetryingBootstrap(true);
    await retryComponentBootstrap();
    setRetryingBootstrap(false);
  };

  const scopeLabel = (app: AppInfo): string => {
    if (app.scope === "global") return "Global";
    const names = gateways.filter((w) => app.scopeGateways.includes(w.id)).map((w) => w.name);
    return names.length > 0 ? names.join(", ") : "—";
  };

  const pluginIds = new Set(plugins.map((p) => p.id));
  const manualSkills = skills.filter((s) => !pluginIds.has(s.id) && !(s.pluginId && pluginIds.has(s.pluginId)));

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <div className="mb-5 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Plugins</h2>
            <p className="m-0 text-[13px] text-muted-foreground">
              Tools and MCP servers your agents can call — attached to every session they're allowed in.
            </p>
          </div>
          <Button variant="outline" onClick={() => setSkillInstall({})}>
            <Sparkles aria-hidden size={14} strokeWidth={2} className="size-3.5" />
            Add skill source
          </Button>
          <Button variant="outline" onClick={() => setAddAppOpen(true)}>
            <Plus aria-hidden size={14} strokeWidth={2} className="size-3.5" />
            Add MCP server
          </Button>
        </div>

        {bootstrapBannerMessage(componentBootstrapStatus) && (
          <Card className="mb-3 flex items-start gap-3 px-[18px] py-3.5">
            <CircleAlert aria-hidden size={16} strokeWidth={2} className="mt-px shrink-0" style={{ color: WARN }} />
            <div className="min-w-0 flex-1">
              <div className="text-[13.5px] font-semibold">Component plugins need attention</div>
              <div className="mt-1 text-[12.5px] text-muted-foreground">{bootstrapBannerMessage(componentBootstrapStatus)}</div>
            </div>
            <Button variant="outline" size="sm" onClick={() => void runRetryBootstrap()} disabled={retryingBootstrap} className="shrink-0">
              <RefreshCw aria-hidden size={13} strokeWidth={2} className={retryingBootstrap ? "animate-spin" : undefined} />
              {retryingBootstrap ? "Retrying…" : "Retry"}
            </Button>
          </Card>
        )}

        {/* Component (WASM bundle) plugins — e.g. mimo/opencode — are never
            `CorePlugin`s (see `store-plugins.ts`'s `FIRST_PARTY_BUNDLE_IDS`
            doc), so they never appear in the Installed/Browse grids below,
            which are both sourced from `listPlugins`. This card is their only
            entry point regardless of which tab is selected, and doubles as
            the "not installed yet" affordance (its detail view owns the
            permission-confirmation install flow). */}
        {componentPluginsLoaded && componentPlugins.length > 0 && (
          <Card className="mb-4 px-[18px] py-4">
            <div className="mb-3 text-sm font-semibold">Component plugins</div>
            <div className="flex flex-col">
              {componentPlugins.map((detail) => (
                <div
                  key={detail.pluginId}
                  className="flex items-center gap-3 border-t border-border py-3 first:border-t-0 first:pt-0 last:pb-0"
                >
                  <IconChip icon={Blocks} size={34} />
                  <div className="min-w-0 flex-1">
                    <div className="overflow-hidden text-ellipsis whitespace-nowrap text-[13px] font-medium">{detail.pluginId}</div>
                    <div className="overflow-hidden text-ellipsis whitespace-nowrap text-[11.5px] text-muted-foreground">
                      {componentPluginStatusLabel(detail)}
                    </div>
                  </div>
                  <Button
                    variant="outline"
                    size="sm"
                    onClick={() => nav.navigate({ kind: "pluginDetail", id: detail.pluginId })}
                    aria-label={`Manage ${detail.pluginId}`}
                  >
                    Manage
                  </Button>
                </div>
              ))}
            </div>
          </Card>
        )}

        <div className="mb-3 flex items-center gap-2">
          <Button
            variant="outline"
            size="sm"
            onClick={() => void runUpdateAll()}
            disabled={updatingAll || installedSkillPacks.length === 0}
          >
            <MonitorUp aria-hidden size={13} strokeWidth={2} className={updatingAll ? "animate-spin" : undefined} />
            {updatingAll ? "Updating…" : "Update all"}
          </Button>
          <Button variant="outline" size="sm" onClick={() => setDoctorOpen(true)}>
            <CircleAlert aria-hidden size={13} strokeWidth={2} style={issueCount > 0 ? { color: WARN } : undefined} />
            {issueCount > 0 ? `${issueCount} issue${issueCount === 1 ? "" : "s"}` : "Doctor: OK"}
          </Button>
          <span className="flex-1" />
        </div>

        <div className="mb-4">
          <Segmented
            options={[
              { id: "installed", label: "Installed" },
              { id: "browse", label: "Browse" },
            ]}
            value={tab}
            onChange={setTab}
          />
        </div>

        {tab === "installed" && (
          <>
            {loaded && pluginsLoaded && apps.length === 0 && installed.length === 0 && manualSkills.length === 0 && (
              <Card className="p-6 text-center text-[13px] text-muted-foreground">
                Nothing installed yet. Browse plugins or add an MCP server by hand.
              </Card>
            )}
            <div className="grid grid-cols-2 gap-3">
              {apps.map((app) => {
                const status = appStatus(app);
                const open = () => nav.navigate({ kind: "appDetail", id: app.id });
                return (
                  <Card key={app.id} className="flex flex-col gap-3 px-[18px] py-4">
                    <Button variant="ghost" onClick={open} className="h-auto w-full justify-start gap-3 p-0 text-left">
                      <Chip initial={app.initial} color={app.color} size={38} mono />
                      <span className="min-w-0 flex-1">
                        <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-sm font-semibold">{app.name}</span>
                        <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-[11.5px] text-muted-foreground">
                          {app.kind}
                        </span>
                      </span>
                      <span className="flex shrink-0 items-center gap-[5px] text-[11px] text-muted-foreground">
                        <StatusDot color={status.color} />
                        {status.label}
                      </span>
                    </Button>
                    <p className="m-0 text-[12.5px] leading-[1.5] text-muted-foreground">{app.desc || "No description."}</p>
                    <div className="flex items-center gap-2 pt-0.5">
                      <Pill variant="mono">{scopeLabel(app)}</Pill>
                      <span className="flex-1" />
                      <Button variant="outline" size="sm" onClick={open}>
                        Configure
                      </Button>
                    </div>
                  </Card>
                );
              })}
              {installed.map((plugin) => (
                <InstalledPluginCard
                  key={plugin.id}
                  plugin={plugin}
                  onOpen={openInstalled(plugin)}
                  onUninstall={() => uninstallPlugin(plugin)}
                  onUpdate={() => void runUpdate(plugin)}
                  onTogglePin={() => runTogglePin(plugin)}
                  pinned={plugin.pinned}
                  attachFailed={attachFailedIds.has(plugin.id)}
                  updating={updatingIds.has(plugin.id)}
                />
              ))}
            </div>
            {manualSkills.length > 0 && (
              <Card className="mt-3 px-[18px] py-4">
                <div className="mb-3 text-sm font-semibold">Skill sources</div>
                <div className="flex flex-col">
                  {manualSkills.map((skill) => (
                    <div
                      key={skill.id}
                      className="flex items-center gap-3 border-t border-border py-3 first:border-t-0 first:pt-0 last:pb-0"
                    >
                      <IconChip icon={Sparkles} size={34} />
                      <div className="min-w-0 flex-1">
                        <div className="overflow-hidden text-ellipsis whitespace-nowrap text-[13px] font-medium">{skill.name}</div>
                        <div className="overflow-hidden text-ellipsis whitespace-nowrap text-[11.5px] text-muted-foreground">
                          {skill.source}
                        </div>
                      </div>
                      <div className="shrink-0 text-[11.5px] text-muted-foreground">{skill.skillCount} skills</div>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => void refreshSkillPack(skill.id)}
                        disabled={skillsLoading}
                        aria-label={`Refresh ${skill.name}`}
                      >
                        <RefreshCw aria-hidden size={13} strokeWidth={2} />
                        Refresh
                      </Button>
                      <Button
                        variant="outline"
                        size="sm"
                        onClick={() => void removeSkillPack(skill.id).then(() => loadPlugins())}
                        disabled={skillsLoading}
                        aria-label={`Remove ${skill.name}`}
                      >
                        <Trash2 aria-hidden size={13} strokeWidth={2} />
                        Remove
                      </Button>
                    </div>
                  ))}
                </div>
              </Card>
            )}
          </>
        )}

        {tab === "browse" && (
          <>
            <div className="mb-4 flex flex-wrap items-center gap-2">
              {categories.length > 0 && (
                <>
                  <span className="text-[12.5px] font-medium text-muted-foreground">Category</span>
                  <Combobox
                    aria-label="Category"
                    options={[{ value: "all", label: "All categories" }, ...categories.map((c) => ({ value: c, label: c }))]}
                    value={category}
                    onValueChange={setCategory}
                    className="w-[200px]"
                  />
                </>
              )}
              <span className="flex-1" />
              {catalogStatus && <span className="text-[11.5px] text-muted-foreground">{catalogStatusLabel(catalogStatus)}</span>}
              <Button variant="outline" size="sm" onClick={() => void runRefreshCatalog()} disabled={refreshingCatalog}>
                <RefreshCw aria-hidden size={13} strokeWidth={2} className={refreshingCatalog ? "animate-spin" : undefined} />
                {refreshingCatalog ? "Refreshing…" : "Refresh catalog"}
              </Button>
            </div>
            {pluginsLoaded && browse.length === 0 && (
              <Card className="mb-3 p-6 text-center text-[13px] text-muted-foreground">
                {browsePlugins(plugins).length === 0 ? "Everything in the catalog is installed." : "No integrations match this category."}
              </Card>
            )}
            <div className="grid grid-cols-2 gap-3">
              {browse.map((plugin) => (
                <BrowseCard key={plugin.id} plugin={plugin} onInstall={() => startInstall(plugin)} />
              ))}
            </div>
          </>
        )}
      </div>
      {addAppOpen && <AddAppModal onClose={() => setAddAppOpen(false)} />}
      {skillInstall && (
        <SkillInstallModal
          initialSource={skillInstall.initialSource}
          onClose={() => {
            setSkillInstall(null);
            void loadPlugins();
            void refreshSkills();
          }}
        />
      )}
      {installingPlugin && (
        <InstallWizardModal
          pluginId={installingPlugin.id}
          pluginName={installingPlugin.name}
          pluginIcon={installingPlugin.icon}
          onClose={() => setInstallingPlugin(null)}
        />
      )}
      <AddConnectionModal
        open={connectingFamily !== null}
        onClose={() => {
          setConnectingFamily(null);
          void loadPlugins();
        }}
        family={connectingFamily ?? ""}
      />
      {doctorOpen && <DoctorPanel onClose={() => setDoctorOpen(false)} />}
    </div>
  );
}
