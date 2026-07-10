import { useEffect, useState } from "react";
import { Check, CircleAlert, Minus, Plus, RefreshCw, Search, Sparkles, Trash2 } from "lucide-react";
import { Badge, Button, Combobox, Input, Segmented, SettingsCard as Card, Switch } from "@ryuzi/ui";
import type { AppInfo, PluginInfo } from "@/bindings";
import { Chip, IconChip, Pill, PluginStatusBadge, StatusDot } from "@/components/common/bits";
import { agentAllowed, useApps } from "@/store-apps";
import { useRuntimes } from "@/store-runtimes";
import { useGateways } from "@/store-gateways";
import { catalogPlugins, usePlugins } from "@/store-plugins";
import { useSkills } from "@/store-skills";
import { pluginIcon } from "@/lib/plugin-icons";
import { AddAppModal } from "@/components/modals/AddAppModal";
import { InstallWizardModal } from "@/components/modals/InstallWizardModal";
import { useNav } from "@/store-nav";

type PluginsTab = "installed" | "access" | "browse" | "skills";

// Plugin name column + one centered toggle column per agent.
const matrixGrid = (n: number) => `minmax(0,1fr) repeat(${n}, 72px)`;

function appStatus(app: AppInfo): { color: string; label: string } {
  if (app.status === "connected") return { color: "#22C55E", label: "Connected" };
  if (app.status === "error") return { color: "#EF4444", label: "Error" };
  return { color: "var(--muted-foreground)", label: "Unchecked" };
}

function colorFor(id: string): string {
  const palette = ["#6C5FC7", "#0FA47F", "#E01E5A", "#3ECF8E", "#A259FF", "#635BFF", "#F46800", "#4285F4"];
  let h = 0;
  for (const c of id) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return palette[h % palette.length];
}

/** Pure so Browse's category filter is unit-testable without mounting the
 *  view — `"all"` (the default) passes every plugin through untouched. */
export function filterByCategory(plugins: PluginInfo[], category: string): PluginInfo[] {
  if (category === "all") return plugins;
  return plugins.filter((p) => p.categories.includes(category));
}

function CatalogCard({
  plugin,
  onOpen,
  onInstall,
  onToggle,
}: {
  plugin: PluginInfo;
  onOpen: () => void;
  onInstall: () => void;
  onToggle: () => void;
}) {
  const Icon = pluginIcon(plugin.icon);
  // Installed = the wizard (or manual config) already ran: configured or
  // enabled. Everything else gets the primary Install entry point.
  const installed = plugin.configured || plugin.enabled;
  return (
    <Card className="flex flex-col gap-3 px-[18px] py-4">
      <div className="flex items-start gap-3">
        <Button variant="ghost" onClick={onOpen} className="h-auto min-w-0 flex-1 justify-start gap-3 p-0 text-left">
          <IconChip icon={Icon} size={38} />
          <span className="min-w-0 flex-1">
            <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-sm font-semibold">{plugin.name}</span>
            <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-[11.5px] text-muted-foreground">
              {plugin.description}
            </span>
          </span>
          <PluginStatusBadge verified={plugin.verified} experimental={plugin.experimental} />
        </Button>
        <Badge variant="outline">Catalog</Badge>
      </div>
      <div className="flex flex-wrap gap-1.5">
        {plugin.categories.map((c) => (
          <Badge key={c} variant="outline">
            {c}
          </Badge>
        ))}
      </div>
      <div className="flex items-center gap-2 pt-0.5">
        <span className="flex-1" />
        {installed ? (
          <>
            <Button variant="outline" size="sm" onClick={onOpen} aria-label={`Open ${plugin.name}`}>
              Open
            </Button>
            <span className={plugin.experimental ? "pointer-events-none opacity-40" : ""}>
              <Switch on={plugin.enabled} onToggle={onToggle} label={`${plugin.name} enabled`} />
            </span>
          </>
        ) : (
          <Button size="sm" onClick={onInstall} aria-label={`Install ${plugin.name}`}>
            Install
          </Button>
        )}
      </div>
    </Card>
  );
}

export function PluginsView() {
  const nav = useNav();
  const { apps, loaded, hydrate, toggleAgent } = useApps();
  const runtimes = useRuntimes((s) => s.runtimes);
  const gateways = useGateways((s) => s.gateways);
  const { plugins, loaded: pluginsLoaded, load: loadPlugins, setEnabled: setPluginEnabled } = usePlugins();
  const skills = useSkills((state) => state.skills);
  const skillsLoading = useSkills((state) => state.loading);
  const skillsError = useSkills((state) => state.error);
  const refreshSkills = useSkills((state) => state.refresh);
  const installSkillSource = useSkills((state) => state.installSource);
  const refreshSkillPack = useSkills((state) => state.refreshSkillPack);
  const removeSkillPack = useSkills((state) => state.remove);
  const [tab, setTab] = useState<PluginsTab>("installed");
  const [category, setCategory] = useState("all");
  const [addOpen, setAddOpen] = useState(false);
  const [installingPlugin, setInstallingPlugin] = useState<PluginInfo | null>(null);
  const [skillInstallSource, setSkillInstallSource] = useState("");
  const [skillsActivated, setSkillsActivated] = useState(false);

  useEffect(() => {
    void hydrate();
  }, [hydrate]);

  useEffect(() => {
    if (!pluginsLoaded) void loadPlugins();
  }, [pluginsLoaded, loadPlugins]);

  useEffect(() => {
    if (tab !== "skills") return;
    if (skillsActivated) return;

    setSkillsActivated(true);
    void refreshSkills();
  }, [refreshSkills, skillsActivated, tab]);

  const catalog = catalogPlugins(plugins);
  const categories = Array.from(new Set(catalog.flatMap((p) => p.categories))).sort();
  const filteredCatalog = filterByCategory(catalog, category);

  const scopeLabel = (app: AppInfo): string => {
    if (app.scope === "global") return "Global";
    const names = gateways.filter((w) => app.scopeGateways.includes(w.id)).map((w) => w.name);
    return names.length > 0 ? names.join(", ") : "—";
  };

  const installCuratedSkill = async (source: string) => {
    const installed = await installSkillSource(source);
    if (installed) {
      setSkillInstallSource("");
    }
  };

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
          <Button variant="outline" onClick={() => setAddOpen(true)}>
            <Plus aria-hidden size={14} strokeWidth={2} className="size-3.5" />
            Add MCP server
          </Button>
          <Button onClick={() => setTab("browse")}>
            <Search aria-hidden size={14} strokeWidth={2} className="size-3.5" />
            Browse plugins
          </Button>
        </div>

        <div className="mb-4">
          <Segmented
            options={[
              { id: "installed", label: "Installed" },
              { id: "access", label: "Access" },
              { id: "browse", label: "Browse" },
              { id: "skills", label: "Skills" },
            ]}
            value={tab}
            onChange={setTab}
          />
        </div>

        {tab !== "browse" && tab !== "skills" && loaded && apps.length === 0 && (
          <Card className="p-6 text-center text-[13px] text-muted-foreground">
            No plugins installed yet. Add an MCP server by hand or browse plugins.
          </Card>
        )}

        {tab === "installed" && (
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
          </div>
        )}

        {tab === "access" && apps.length > 0 && (
          <>
            <Card>
              <div
                className="grid items-center border-b border-border px-[18px] py-2.5"
                style={{ gridTemplateColumns: matrixGrid(runtimes.length) }}
              >
                <span className="text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground">Plugin</span>
                {runtimes.map((a) => (
                  <span key={a.id} className="flex items-center justify-center gap-1.5 text-[11.5px] font-semibold">
                    <StatusDot color={a.color} />
                    {a.name.split(" ")[0]}
                  </span>
                ))}
              </div>
              {apps.map((app) => (
                <div
                  key={app.id}
                  className="grid items-center border-b border-border px-[18px] py-[9px] last:border-b-0"
                  style={{ gridTemplateColumns: matrixGrid(runtimes.length) }}
                >
                  <span className="flex min-w-0 items-center gap-2.5">
                    <Chip initial={app.initial} color={app.color} size={26} mono />
                    <span className="overflow-hidden text-ellipsis whitespace-nowrap text-[13px] font-medium">{app.name}</span>
                  </span>
                  {runtimes.map((a) => {
                    const on = agentAllowed(app, a.id);
                    return (
                      <span key={a.id} className="flex justify-center">
                        <Button
                          variant="outline"
                          size="icon-sm"
                          aria-label={`${on ? "Block" : "Allow"} ${app.name} for ${a.name}`}
                          onClick={() => void toggleAgent(app.id, a.id, !on)}
                          className="text-muted-foreground"
                        >
                          {on ? (
                            <Check aria-hidden size={13} strokeWidth={2.5} className="size-[13px]" style={{ color: "#22C55E" }} />
                          ) : (
                            <Minus aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
                          )}
                        </Button>
                      </span>
                    );
                  })}
                </div>
              ))}
            </Card>
            <p className="mx-0.5 mb-0 mt-2.5 text-xs text-muted-foreground">
              Access here applies before per-tool permissions — a blocked agent never sees the plugin's tools.
            </p>
          </>
        )}

        {tab === "browse" && (
          <>
            {categories.length > 0 && (
              <div className="mb-4 flex flex-wrap items-center gap-2">
                <span className="text-[12.5px] font-medium text-muted-foreground">Category</span>
                <Combobox
                  aria-label="Category"
                  options={[{ value: "all", label: "All categories" }, ...categories.map((c) => ({ value: c, label: c }))]}
                  value={category}
                  onValueChange={setCategory}
                  className="w-[200px]"
                />
              </div>
            )}

            {pluginsLoaded && catalog.length === 0 && (
              <Card className="mb-3 p-6 text-center text-[13px] text-muted-foreground">No catalog integrations available yet.</Card>
            )}

            {pluginsLoaded && catalog.length > 0 && filteredCatalog.length === 0 && (
              <Card className="mb-3 p-6 text-center text-[13px] text-muted-foreground">No integrations match this category.</Card>
            )}

            <div className="grid grid-cols-2 gap-3">
              {filteredCatalog.map((plugin) => (
                <CatalogCard
                  key={plugin.id}
                  plugin={plugin}
                  onOpen={() => {
                    // Single-flight guard: with the wizard open, the Modal
                    // scrim blocks mouse clicks on background cards, but a
                    // keyboard user can still Tab past it and press Enter
                    // on another card's Open/Install/Switch — navigating
                    // away mid-wizard is the same class of bypass, so
                    // no-op while installingPlugin is set.
                    if (installingPlugin) return;
                    nav.navigate({ kind: "pluginDetail", id: plugin.id });
                  }}
                  onInstall={() => {
                    // Same single-flight guard: don't let a background
                    // Install swap installingPlugin mid-wizard.
                    if (installingPlugin) return;
                    setInstallingPlugin(plugin);
                  }}
                  onToggle={() => {
                    // Same single-flight guard: don't let a background
                    // Switch toggle fire while the wizard is open.
                    if (installingPlugin) return;
                    if (!plugin.experimental) void setPluginEnabled(plugin.id, !plugin.enabled);
                  }}
                />
              ))}
            </div>
          </>
        )}

        {tab === "skills" && (
          <div className="flex flex-col gap-3">
            <Card className="px-[18px] py-4">
              <div className="flex items-center gap-3">
                <IconChip icon={Sparkles} size={38} />
                <div className="min-w-0 flex-1">
                  <div className="text-sm font-semibold">Superpowers</div>
                  <div className="text-[11.5px] text-muted-foreground">Curated workflow and development skills</div>
                </div>
                <Button
                  size="sm"
                  onClick={() => void installCuratedSkill("superpowers")}
                  disabled={skillsLoading}
                  aria-label="Install Superpowers"
                >
                  Install
                </Button>
              </div>
            </Card>

            <Card className="px-[18px] py-4">
              <div className="flex flex-wrap items-center gap-2">
                <div className="min-w-0 flex-1 text-sm font-semibold">Install source</div>
                <Button variant="outline" size="sm" onClick={() => void refreshSkills()} disabled={skillsLoading}>
                  <RefreshCw aria-hidden size={13} strokeWidth={2} />
                  Refresh list
                </Button>
              </div>
              <div className="mt-3 flex items-center gap-2">
                <Input
                  value={skillInstallSource}
                  onChange={(event) => setSkillInstallSource(event.target.value)}
                  placeholder="superpowers or owner/repo"
                  aria-label="Skill source"
                  className="flex-1"
                />
                <Button
                  onClick={() => void installCuratedSkill(skillInstallSource)}
                  disabled={skillsLoading || skillInstallSource.trim() === ""}
                >
                  Install source
                </Button>
              </div>
            </Card>

            {skillsError && (
              <div className="flex items-center gap-2 rounded-md border border-border px-4 py-3 text-[12.5px]" style={{ color: "#F59E0B" }}>
                <CircleAlert aria-hidden size={14} strokeWidth={2} className="shrink-0" />
                {skillsError}
              </div>
            )}

            <Card className="px-[18px] py-4">
              <div className="mb-3 flex items-center gap-2">
                <div className="min-w-0 flex-1 text-sm font-semibold">Installed</div>
                {skillsLoading && <div className="text-[11.5px] text-muted-foreground">Loading…</div>}
              </div>

              {skills.length === 0 && !skillsLoading ? (
                <div className="text-[12.5px] text-muted-foreground">No skill packs installed.</div>
              ) : (
                <div className="flex flex-col">
                  {skills.map((skill) => (
                    <div
                      key={skill.id}
                      className="flex items-center gap-3 border-t border-border py-3 first:border-t-0 first:pt-0 last:pb-0"
                    >
                      <Chip initial={skill.name.charAt(0).toUpperCase()} color={colorFor(skill.id)} size={34} mono />
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
                        onClick={() => void removeSkillPack(skill.id)}
                        disabled={skillsLoading}
                        aria-label={`Remove ${skill.name}`}
                      >
                        <Trash2 aria-hidden size={13} strokeWidth={2} />
                        Remove
                      </Button>
                    </div>
                  ))}
                </div>
              )}
            </Card>
          </div>
        )}
      </div>
      {addOpen && <AddAppModal onClose={() => setAddOpen(false)} />}
      {installingPlugin && (
        <InstallWizardModal
          pluginId={installingPlugin.id}
          pluginName={installingPlugin.name}
          pluginIcon={installingPlugin.icon}
          onClose={() => setInstallingPlugin(null)}
        />
      )}
    </div>
  );
}
