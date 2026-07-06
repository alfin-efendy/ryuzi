import { useEffect, useState } from "react";
import { Check, Minus, Plus, Store } from "lucide-react";
import { Badge, Button, Combobox, Segmented, SettingsCard as Card, Switch } from "@ryuzi/ui";
import { Chip, IconChip, Pill, PluginStatusBadge, StatusDot } from "@/components/common/bits";
import type { AppInfo, PluginInfo } from "@/bindings";
import { agentAllowed, useApps } from "@/store-apps";
import { useRuntimes } from "@/store-runtimes";
import { useGateways } from "@/store-gateways";
import { catalogPlugins, usePlugins } from "@/store-plugins";
import { pluginIcon } from "@/lib/plugin-icons";
import { AddAppModal } from "@/components/modals/AddAppModal";
import { useNav } from "@/store-nav";

// App name column + one centered toggle column per agent.
const matrixGrid = (n: number) => `minmax(0,1fr) repeat(${n}, 72px)`;

function appStatus(app: AppInfo): { color: string; label: string } {
  if (app.status === "connected") return { color: "#22C55E", label: "Connected" };
  if (app.status === "error") return { color: "#EF4444", label: "Error" };
  return { color: "var(--muted-foreground)", label: "Unchecked" };
}

/** Pure so the Catalog tab's category filter is unit-testable without mounting
 *  the view — `"all"` (the default) passes every plugin through untouched. */
export function filterByCategory(plugins: PluginInfo[], category: string): PluginInfo[] {
  if (category === "all") return plugins;
  return plugins.filter((p) => p.categories.includes(category));
}

function CatalogCard({ plugin, onOpen, onToggle }: { plugin: PluginInfo; onOpen: () => void; onToggle: () => void }) {
  const Icon = pluginIcon(plugin.icon);
  return (
    <Card className="flex flex-col gap-3 px-[18px] py-4">
      <Button variant="ghost" onClick={onOpen} className="h-auto w-full justify-start gap-3 p-0 text-left">
        <IconChip icon={Icon} size={38} />
        <span className="min-w-0 flex-1">
          <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-sm font-semibold">{plugin.name}</span>
          <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-[11.5px] text-muted-foreground">
            {plugin.description}
          </span>
        </span>
        <PluginStatusBadge verified={plugin.verified} experimental={plugin.experimental} />
      </Button>
      <div className="flex flex-wrap gap-1.5">
        {plugin.categories.map((c) => (
          <Badge key={c} variant="outline">
            {c}
          </Badge>
        ))}
      </div>
      <div className="flex items-center gap-2 pt-0.5">
        <span className="flex-1" />
        <Button variant="outline" size="sm" onClick={onOpen}>
          Configure
        </Button>
        <span className={plugin.experimental ? "pointer-events-none opacity-40" : ""}>
          <Switch on={plugin.enabled} onToggle={onToggle} label={`${plugin.name} enabled`} />
        </span>
      </div>
    </Card>
  );
}

export function AppsView() {
  const nav = useNav();
  const { apps, loaded, hydrate, toggleAgent } = useApps();
  const runtimes = useRuntimes((s) => s.runtimes);
  const gateways = useGateways((s) => s.gateways);
  const { plugins, loaded: pluginsLoaded, load: loadPlugins, setEnabled: setPluginEnabled } = usePlugins();
  const [tab, setTab] = useState<"installed" | "access" | "catalog">("installed");
  const [category, setCategory] = useState("all");
  const [addOpen, setAddOpen] = useState(false);

  useEffect(() => {
    void hydrate();
  }, [hydrate]);

  useEffect(() => {
    if (!pluginsLoaded) void loadPlugins();
  }, [pluginsLoaded, loadPlugins]);

  const catalog = catalogPlugins(plugins);
  const categories = Array.from(new Set(catalog.flatMap((p) => p.categories))).sort();
  const filteredCatalog = filterByCategory(catalog, category);

  const scopeLabel = (app: AppInfo): string => {
    if (app.scope === "global") return "Global";
    const names = gateways.filter((w) => app.scopeGateways.includes(w.id)).map((w) => w.name);
    return names.length > 0 ? names.join(", ") : "—";
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <div className="mb-5 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Apps</h2>
            <p className="m-0 text-[13px] text-muted-foreground">
              Tools and MCP servers your agents can call — attached to every session they're allowed in.
            </p>
          </div>
          <Button variant="outline" onClick={() => setAddOpen(true)}>
            <Plus aria-hidden size={14} strokeWidth={2} className="size-3.5" />
            Add app
          </Button>
          <Button onClick={() => nav.navigate({ kind: "registry" })}>
            <Store aria-hidden size={14} strokeWidth={2} className="size-3.5" />
            Browse registry
          </Button>
        </div>

        <div className="mb-4">
          <Segmented
            options={[
              { id: "installed", label: "Installed" },
              { id: "access", label: "Access" },
              { id: "catalog", label: "Catalog" },
            ]}
            value={tab}
            onChange={setTab}
          />
        </div>

        {tab !== "catalog" && loaded && apps.length === 0 && (
          <Card className="p-6 text-center text-[13px] text-muted-foreground">
            No apps installed yet. Add an MCP server by hand or browse the registry.
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
                <span className="text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground">App</span>
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
              Access here applies before per-tool permissions — a blocked agent never sees the app’s tools.
            </p>
          </>
        )}

        {tab === "catalog" && (
          <>
            {categories.length > 0 && (
              <div className="mb-4 flex items-center gap-2">
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
              <Card className="p-6 text-center text-[13px] text-muted-foreground">No catalog integrations available yet.</Card>
            )}

            {pluginsLoaded && catalog.length > 0 && filteredCatalog.length === 0 && (
              <Card className="p-6 text-center text-[13px] text-muted-foreground">No integrations match this category.</Card>
            )}

            {filteredCatalog.length > 0 && (
              <div className="grid grid-cols-2 gap-3">
                {filteredCatalog.map((plugin) => (
                  <CatalogCard
                    key={plugin.id}
                    plugin={plugin}
                    onOpen={() => nav.navigate({ kind: "pluginDetail", id: plugin.id })}
                    onToggle={() => {
                      if (!plugin.experimental) void setPluginEnabled(plugin.id, !plugin.enabled);
                    }}
                  />
                ))}
              </div>
            )}
          </>
        )}
      </div>
      {addOpen && <AddAppModal onClose={() => setAddOpen(false)} />}
    </div>
  );
}
