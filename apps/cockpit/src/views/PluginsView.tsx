import { type ReactNode, useCallback, useEffect, useMemo, useState } from "react";
import { Check, CircleAlert, Minus, Plus, RefreshCw, Search, Sparkles, Trash2 } from "lucide-react";
import { toast } from "sonner";
import { Badge, Button, Combobox, Input, Segmented, SettingsCard as Card, Switch } from "@ryuzi/ui";
import { commands, type AppInfo, type PluginInfo, type RegistryEntry, type RegistryEntryVersion } from "@/bindings";
import { Chip, IconChip, Pill, PluginStatusBadge, StatusDot } from "@/components/common/bits";
import { agentAllowed, useApps } from "@/store-apps";
import { useRuntimes } from "@/store-runtimes";
import { useGateways } from "@/store-gateways";
import { catalogPlugins, usePlugins } from "@/store-plugins";
import { useSkills } from "@/store-skills";
import { pluginIcon } from "@/lib/plugin-icons";
import { AddAppModal } from "@/components/modals/AddAppModal";
import { useNav } from "@/store-nav";

type PluginsTab = "installed" | "access" | "browse" | "skills";
type BrowseSource = "all" | "catalog" | "registry";

const QUICK_SEARCHES = ["All", "github", "database", "browser", "docs", "monitoring", "payments", "deploy"];

// Plugin name column + one centered toggle column per agent.
const matrixGrid = (n: number) => `minmax(0,1fr) repeat(${n}, 72px)`;

function appStatus(app: AppInfo): { color: string; label: string } {
  if (app.status === "connected") return { color: "#22C55E", label: "Connected" };
  if (app.status === "error") return { color: "#EF4444", label: "Error" };
  return { color: "var(--muted-foreground)", label: "Unchecked" };
}

function parseVersionPart(part: string): { kind: "numeric"; value: number } | { kind: "string"; value: string } {
  if (/^\d+$/.test(part)) {
    return { kind: "numeric", value: Number(part) };
  }
  return { kind: "string", value: part };
}

function compareVersions(left: string, right: string): number {
  const leftParts = left.split(".");
  const rightParts = right.split(".");
  const maxParts = Math.max(leftParts.length, rightParts.length);

  for (let i = 0; i < maxParts; i++) {
    const leftRaw = leftParts[i] ?? "0";
    const rightRaw = rightParts[i] ?? "0";
    const leftParsed = parseVersionPart(leftRaw);
    const rightParsed = parseVersionPart(rightRaw);

    if (leftParsed.kind === "numeric" && rightParsed.kind === "numeric") {
      if (leftParsed.value !== rightParsed.value) {
        return leftParsed.value - rightParsed.value;
      }
      continue;
    }

    const leftValue = leftParsed.kind === "numeric" ? String(leftParsed.value) : leftParsed.value;
    const rightValue = rightParsed.kind === "numeric" ? String(rightParsed.value) : rightParsed.value;
    if (leftValue === rightValue) continue;
    return leftValue < rightValue ? -1 : 1;
  }

  return 0;
}

function mergeVersions(existing: RegistryEntryVersion[], incoming: RegistryEntryVersion[]): RegistryEntryVersion[] {
  const byVersion = new Map<string, RegistryEntryVersion>();

  for (const version of existing) {
    byVersion.set(version.version, { ...version });
  }

  for (const version of incoming) {
    const existingVersion = byVersion.get(version.version);

    if (!existingVersion) {
      byVersion.set(version.version, { ...version });
      continue;
    }

    if (version.isLatest || !existingVersion.isLatest) {
      byVersion.set(version.version, { ...version });
    }
  }

  const merged = Array.from(byVersion.values());

  merged.sort((left, right) => {
    if (left.isLatest !== right.isLatest) {
      return left.isLatest ? -1 : 1;
    }

    const versionCmp = compareVersions(right.version, left.version);
    if (versionCmp !== 0) {
      return versionCmp;
    }

    return right.version.localeCompare(left.version);
  });

  return merged;
}

function pickTopLevelSource(existing: RegistryEntry, incoming: RegistryEntry, winningVersion: string): RegistryEntry | null {
  if (incoming.version === winningVersion) return incoming;
  if (existing.version === winningVersion) return existing;
  return null;
}

function setTopLevelFromWinner(entry: RegistryEntry, winner: RegistryEntryVersion | undefined, source: RegistryEntry | null) {
  if (!winner) return;

  if (source) {
    entry.name = source.name;
    entry.desc = source.desc;
    entry.publisher = source.publisher;
    entry.kind = source.kind;
  }

  entry.version = winner.version;
  entry.installTarget = winner.installTarget;
  entry.website = winner.website;
}

export function mergeRegistryEntries(prev: RegistryEntry[], next: RegistryEntry[]): RegistryEntry[] {
  const out = [...prev.map((entry) => ({ ...entry, versions: [...entry.versions] }))];
  const entryById = new Map<string, RegistryEntry>(out.map((entry) => [entry.id, entry]));

  for (const entry of next) {
    const existing = entryById.get(entry.id);

    if (!existing) {
      out.push({ ...entry, versions: [...entry.versions] });
      entryById.set(entry.id, out[out.length - 1]);
      continue;
    }

    const mergedVersions = mergeVersions(existing.versions, entry.versions);
    const winner = mergedVersions[0];
    const winnerSource = pickTopLevelSource(existing, entry, winner.version);

    Object.assign(existing, {
      versions: mergedVersions,
    });
    setTopLevelFromWinner(existing, winner, winnerSource);
  }

  return out;
}

function colorFor(id: string): string {
  const palette = ["#6C5FC7", "#0FA47F", "#E01E5A", "#3ECF8E", "#A259FF", "#635BFF", "#F46800", "#4285F4"];
  let h = 0;
  for (const c of id) h = (h * 31 + c.charCodeAt(0)) >>> 0;
  return palette[h % palette.length];
}

function registryKindLabel(kind: string): string {
  return kind === "http" ? "Remote" : "npm";
}

function buildRegistryQuery(query: string, quick: string): string | null {
  const search = quick === "All" ? query.trim() : query.trim() ? `${quick} ${query.trim()}` : quick;
  return search === "" ? null : search;
}

/** Pure so Browse's category filter is unit-testable without mounting the
 *  view — `"all"` (the default) passes every plugin through untouched. */
export function filterByCategory(plugins: PluginInfo[], category: string): PluginInfo[] {
  if (category === "all") return plugins;
  return plugins.filter((p) => p.categories.includes(category));
}

function CatalogCard({ plugin, onOpen, onToggle }: { plugin: PluginInfo; onOpen: () => void; onToggle: () => void }) {
  const Icon = pluginIcon(plugin.icon);
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

function RegistryCard({
  entry,
  installed,
  installing,
  selectedVersion,
  onSelectVersion,
  onInstall,
}: {
  entry: RegistryEntry;
  installed: boolean;
  installing: boolean;
  selectedVersion: string;
  onSelectVersion: (version: string) => void;
  onInstall: () => void;
}) {
  const version = entry.versions.find((item) => item.version === selectedVersion) ?? entry.versions[0];
  const installTarget = version?.installTarget ?? entry.installTarget;

  let action: ReactNode;
  if (installed) {
    action = (
      <span className="flex h-[27px] items-center gap-1.5 px-[11px] text-xs font-medium" style={{ color: "#22C55E" }}>
        <Check aria-hidden size={13} strokeWidth={2.5} />
        Installed
      </span>
    );
  } else if (installing) {
    action = (
      <span className="flex h-[27px] items-center gap-[7px] px-[11px] text-xs text-muted-foreground">
        <StatusDot color="#3B82F6" size={8} pulse />
        Installing…
      </span>
    );
  } else {
    action = (
      <Button size="sm" onClick={onInstall} disabled={!installTarget} aria-label={`Install ${entry.name}`}>
        Install
      </Button>
    );
  }

  return (
    <Card className="flex flex-col gap-3 px-[18px] py-4">
      <div className="flex items-start gap-3">
        <Chip initial={entry.name.charAt(0).toUpperCase()} color={colorFor(entry.id)} size={38} mono />
        <div className="min-w-0 flex-1">
          <div className="overflow-hidden text-ellipsis whitespace-nowrap text-sm font-semibold">{entry.name}</div>
          <div className="overflow-hidden text-ellipsis whitespace-nowrap text-[11.5px] text-muted-foreground">{entry.publisher}</div>
        </div>
        <Badge variant="outline">Registry</Badge>
      </div>
      <p className="m-0 line-clamp-2 min-h-[38px] text-[12.5px] leading-[1.5] text-muted-foreground">{entry.desc}</p>
      <div className="flex items-center gap-2">
        <Pill variant="mono">{registryKindLabel(entry.kind)}</Pill>
        <span className="flex-1" />
        {entry.versions.length > 1 ? (
          <Combobox
            aria-label={`Version for ${entry.name}`}
            options={entry.versions.map((item) => ({ value: item.version, label: item.version }))}
            value={selectedVersion}
            onValueChange={onSelectVersion}
            className="h-8 w-[116px]"
          />
        ) : (
          <span className="shrink-0 font-mono text-[11px] text-muted-foreground">v{version?.version ?? entry.version}</span>
        )}
      </div>
      <div className="flex items-center gap-2 pt-0.5">
        {version?.website && (
          <a
            href={version.website}
            target="_blank"
            rel="noreferrer"
            className="text-[12px] text-muted-foreground underline-offset-4 hover:underline"
          >
            Website
          </a>
        )}
        <span className="flex-1" />
        {action}
      </div>
    </Card>
  );
}

export function PluginsView() {
  const nav = useNav();
  const { apps, loaded, hydrate, add, toggleAgent } = useApps();
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
  const [browseSource, setBrowseSource] = useState<BrowseSource>("all");
  const [query, setQuery] = useState("");
  const [quick, setQuick] = useState("All");
  const [registryEntries, setRegistryEntries] = useState<RegistryEntry[]>([]);
  const [registryLoading, setRegistryLoading] = useState(false);
  const [registryError, setRegistryError] = useState<string | null>(null);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [installing, setInstalling] = useState<string | null>(null);
  const [selectedVersions, setSelectedVersions] = useState<Record<string, string>>({});
  const [registryActivated, setRegistryActivated] = useState(false);
  const [addOpen, setAddOpen] = useState(false);
  const [skillInstallSource, setSkillInstallSource] = useState("");
  const [skillsActivated, setSkillsActivated] = useState(false);

  useEffect(() => {
    void hydrate();
  }, [hydrate]);

  useEffect(() => {
    if (!pluginsLoaded) void loadPlugins();
  }, [pluginsLoaded, loadPlugins]);

  const loadRegistry = useCallback(async (search: string | null, cursor: string | null) => {
    setRegistryLoading(true);
    setRegistryError(null);

    const res = await commands.registrySearch(search, cursor);

    setRegistryLoading(false);
    if (res.status === "error") {
      setRegistryError(res.error.message);
      return;
    }

    setRegistryEntries((prev) => (cursor ? mergeRegistryEntries(prev, res.data.entries) : mergeRegistryEntries([], res.data.entries)));
    setNextCursor(res.data.nextCursor);
  }, []);

  useEffect(() => {
    if (tab !== "browse") return;
    if (registryActivated) return;

    setRegistryActivated(true);
    void loadRegistry(buildRegistryQuery(query, quick), null);
  }, [loadRegistry, query, quick, registryActivated, tab]);

  useEffect(() => {
    if (tab !== "browse" || !registryActivated) return;

    const timeout = setTimeout(() => {
      void loadRegistry(buildRegistryQuery(query, quick), null);
    }, 350);

    return () => clearTimeout(timeout);
  }, [loadRegistry, query, quick, registryActivated, tab]);

  useEffect(() => {
    if (tab !== "skills") return;
    if (skillsActivated) return;

    setSkillsActivated(true);
    void refreshSkills();
  }, [refreshSkills, skillsActivated, tab]);

  const catalog = catalogPlugins(plugins);
  const categories = Array.from(new Set(catalog.flatMap((p) => p.categories))).sort();
  const filteredCatalog = filterByCategory(catalog, category);
  const showCatalog = browseSource !== "registry";
  const showRegistry = browseSource !== "catalog";
  const installedNames = useMemo(() => new Set(apps.map((app) => app.name.toLowerCase())), [apps]);

  const scopeLabel = (app: AppInfo): string => {
    if (app.scope === "global") return "Global";
    const names = gateways.filter((w) => app.scopeGateways.includes(w.id)).map((w) => w.name);
    return names.length > 0 ? names.join(", ") : "—";
  };

  const installRegistryEntry = async (entry: RegistryEntry) => {
    const selectedVersion = selectedVersions[entry.id] ?? entry.version;
    const version = entry.versions.find((item) => item.version === selectedVersion) ?? entry.versions[0];
    const installTarget = version?.installTarget ?? entry.installTarget;

    if (!installTarget) {
      toast.error("This entry has no installable package or remote URL.");
      return;
    }

    setInstalling(entry.id);
    const ok = await add({
      id: null,
      name: entry.name,
      description: entry.desc,
      kind: "MCP server",
      transport: entry.kind === "http" ? "http" : "stdio",
      command: entry.kind === "http" ? null : "npx",
      args: entry.kind === "http" ? [] : ["-y", installTarget],
      env: [],
      url: entry.kind === "http" ? installTarget : null,
      version: version?.version ?? entry.version,
      publisher: entry.publisher,
      color: colorFor(entry.id),
    });
    setInstalling(null);

    if (ok) {
      toast.success(`${entry.name} installed — check its status in Plugins`);
    }
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
            <div className="mb-4 flex flex-wrap items-center gap-2">
              <span className="text-[12.5px] font-medium text-muted-foreground">Source</span>
              <Segmented
                size="sm"
                options={[
                  { id: "all", label: "All" },
                  { id: "catalog", label: "Catalog" },
                  { id: "registry", label: "Registry" },
                ]}
                value={browseSource}
                onChange={setBrowseSource}
              />
              {showCatalog && categories.length > 0 && (
                <>
                  <span className="ml-2 text-[12.5px] font-medium text-muted-foreground">Category</span>
                  <Combobox
                    aria-label="Category"
                    options={[{ value: "all", label: "All categories" }, ...categories.map((c) => ({ value: c, label: c }))]}
                    value={category}
                    onValueChange={setCategory}
                    className="w-[200px]"
                  />
                </>
              )}
            </div>

            {showRegistry && (
              <>
                <div className="mb-3 flex h-[34px] w-full max-w-[380px] items-center gap-2 rounded-md border border-input bg-background px-3 text-muted-foreground">
                  <Search aria-hidden size={13} strokeWidth={2} className="shrink-0" />
                  <Input
                    value={query}
                    onChange={(e) => setQuery(e.target.value)}
                    placeholder="Search the registry"
                    aria-label="Search the registry"
                    className="h-auto min-w-0 flex-1 border-none bg-transparent p-0 text-foreground focus-visible:ring-0 dark:bg-transparent"
                  />
                </div>

                <div className="mb-4 flex flex-wrap gap-1.5">
                  {QUICK_SEARCHES.map((item) => {
                    const selected = item === quick;
                    return (
                      <Button
                        key={item}
                        variant={selected ? "default" : "outline"}
                        size="xs"
                        onClick={() => setQuick(item)}
                        className="rounded-full px-3 capitalize"
                      >
                        {item}
                      </Button>
                    );
                  })}
                </div>

                {registryError && (
                  <div
                    className="mb-4 flex items-center gap-2 rounded-md border border-border px-4 py-3 text-[12.5px]"
                    style={{ color: "#F59E0B" }}
                  >
                    <CircleAlert aria-hidden size={14} strokeWidth={2} className="shrink-0" />
                    {registryError}
                  </div>
                )}
              </>
            )}

            {showCatalog && pluginsLoaded && catalog.length === 0 && (
              <Card className="mb-3 p-6 text-center text-[13px] text-muted-foreground">No catalog integrations available yet.</Card>
            )}

            {showCatalog && pluginsLoaded && catalog.length > 0 && filteredCatalog.length === 0 && (
              <Card className="mb-3 p-6 text-center text-[13px] text-muted-foreground">No integrations match this category.</Card>
            )}

            {showRegistry && !registryLoading && !registryError && registryEntries.length === 0 && (
              <Card className="mb-3 p-6 text-center text-[13px] text-muted-foreground">No registry results found.</Card>
            )}

            <div className="grid grid-cols-2 gap-3">
              {showCatalog &&
                filteredCatalog.map((plugin) => (
                  <CatalogCard
                    key={plugin.id}
                    plugin={plugin}
                    onOpen={() => nav.navigate({ kind: "pluginDetail", id: plugin.id })}
                    onToggle={() => {
                      if (!plugin.experimental) void setPluginEnabled(plugin.id, !plugin.enabled);
                    }}
                  />
                ))}
              {showRegistry &&
                registryEntries.map((entry) => (
                  <RegistryCard
                    key={entry.id}
                    entry={entry}
                    installed={installedNames.has(entry.name.toLowerCase())}
                    installing={installing === entry.id}
                    selectedVersion={selectedVersions[entry.id] ?? entry.version}
                    onSelectVersion={(version) => setSelectedVersions((prev) => ({ ...prev, [entry.id]: version }))}
                    onInstall={() => void installRegistryEntry(entry)}
                  />
                ))}
            </div>

            {showRegistry && registryLoading && <div className="py-6 text-center text-[13px] text-muted-foreground">Searching…</div>}

            {showRegistry && !registryLoading && nextCursor && (
              <div className="mt-4 flex justify-center">
                <Button variant="outline" onClick={() => void loadRegistry(buildRegistryQuery(query, quick), nextCursor)} className="px-4">
                  Load more
                </Button>
              </div>
            )}
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
    </div>
  );
}
