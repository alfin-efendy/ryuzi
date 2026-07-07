import { type ReactNode, useCallback, useEffect, useState } from "react";
import { Check, CircleAlert, Search } from "lucide-react";
import { toast } from "sonner";
import { Button, Input, SettingsCard as Card } from "@ryuzi/ui";
import { commands, type RegistryEntry, type RegistryEntryVersion } from "@/bindings";
import { BackButton } from "@/components/common/DetailHeader";
import { Chip, Pill, StatusDot } from "@/components/common/bits";
import { useApps } from "@/store-apps";
import { useNav } from "@/store-nav";

// Curated quick searches against the live registry (it has no category API).
const QUICK_SEARCHES = ["All", "github", "database", "browser", "docs", "monitoring", "payments", "deploy"];

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

export function RegistryView() {
  const nav = useNav();
  const { apps, hydrate: hydrateApps, add } = useApps();
  const [query, setQuery] = useState("");
  const [quick, setQuick] = useState("All");
  const [entries, setEntries] = useState<RegistryEntry[]>([]);
  const [nextCursor, setNextCursor] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [installing, setInstalling] = useState<string | null>(null);

  const runSearch = useCallback(async (q: string, cursor: string | null) => {
    setLoading(true);
    setError(null);
    const res = await commands.registrySearch(q.trim() || null, cursor);
    setLoading(false);
    if (res.status === "error") {
      setError(res.error.message);
      return;
    }
    setEntries((prev) => (cursor ? mergeRegistryEntries(prev, res.data.entries) : mergeRegistryEntries([], res.data.entries)));
    setNextCursor(res.data.nextCursor);
  }, []);

  useEffect(() => {
    void hydrateApps();
    void runSearch("", null);
  }, [hydrateApps, runSearch]);

  // Debounced live search.
  useEffect(() => {
    const q = quick === "All" ? query : query.trim() ? `${quick} ${query}` : quick;
    const t = setTimeout(() => void runSearch(q, null), 350);
    return () => clearTimeout(t);
  }, [query, quick, runSearch]);

  const install = async (entry: RegistryEntry) => {
    if (!entry.installTarget) {
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
      args: entry.kind === "http" ? [] : ["-y", entry.installTarget],
      env: [],
      url: entry.kind === "http" ? entry.installTarget : null,
      version: entry.version,
      publisher: entry.publisher,
      color: colorFor(entry.id),
    });
    setInstalling(null);
    if (ok) toast.success(`${entry.name} installed — check its status in Apps`);
  };

  const installedNames = new Set(apps.map((a) => a.name.toLowerCase()));

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[860px]">
        <BackButton label="Apps" onClick={() => nav.navigate({ kind: "apps" })} />

        <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Registry</h2>
        <p className="m-0 mb-4 text-[13px] text-muted-foreground">
          Live search of the official MCP registry. Installing adds the server to Apps and connects immediately.
        </p>

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
          {QUICK_SEARCHES.map((c) => {
            const sel = c === quick;
            return (
              <Button
                key={c}
                variant={sel ? "default" : "outline"}
                size="xs"
                onClick={() => setQuick(c)}
                className="rounded-full px-3 capitalize"
              >
                {c}
              </Button>
            );
          })}
        </div>

        {error && (
          <div
            className="mb-4 flex items-center gap-2 rounded-md border border-border px-4 py-3 text-[12.5px]"
            style={{ color: "#F59E0B" }}
          >
            <CircleAlert aria-hidden size={14} strokeWidth={2} className="shrink-0" />
            {error}
          </div>
        )}

        {!loading && !error && entries.length === 0 && <div className="py-8 text-[13px] text-muted-foreground">No results found.</div>}

        <div className="grid grid-cols-2 gap-3">
          {entries.map((rg) => {
            const installed = installedNames.has(rg.name.toLowerCase());
            let action: ReactNode;
            if (installed) {
              action = (
                <span className="flex h-[27px] items-center gap-1.5 px-[11px] text-xs font-medium" style={{ color: "#22C55E" }}>
                  <Check aria-hidden size={13} strokeWidth={2.5} />
                  Installed
                </span>
              );
            } else if (installing === rg.id) {
              action = (
                <span className="flex h-[27px] items-center gap-[7px] px-[11px] text-xs text-muted-foreground">
                  <StatusDot color="#3B82F6" size={8} pulse />
                  Installing…
                </span>
              );
            } else {
              action = (
                <Button size="sm" onClick={() => void install(rg)} disabled={!rg.installTarget}>
                  Install
                </Button>
              );
            }
            return (
              <Card key={rg.id} className="flex flex-col gap-3 px-[18px] py-4">
                <div className="flex items-center gap-3">
                  <Chip initial={rg.name.charAt(0).toUpperCase()} color={colorFor(rg.id)} size={38} mono />
                  <div className="min-w-0 flex-1">
                    <div className="overflow-hidden text-ellipsis whitespace-nowrap text-sm font-semibold">{rg.name}</div>
                    <div className="overflow-hidden text-ellipsis whitespace-nowrap text-[11.5px] text-muted-foreground">
                      {rg.publisher}
                    </div>
                  </div>
                  {rg.version && <span className="shrink-0 font-mono text-[11px] text-muted-foreground">v{rg.version}</span>}
                </div>
                <p className="m-0 line-clamp-2 min-h-[38px] text-[12.5px] leading-[1.5] text-muted-foreground">{rg.desc}</p>
                <div className="flex items-center gap-2 pt-0.5">
                  <Pill variant="mono">{rg.kind === "http" ? "Remote" : "npm"}</Pill>
                  <span className="flex-1" />
                  {action}
                </div>
              </Card>
            );
          })}
        </div>

        {loading && <div className="py-6 text-center text-[13px] text-muted-foreground">Searching…</div>}
        {!loading && nextCursor && (
          <div className="mt-4 flex justify-center">
            <Button
              variant="outline"
              onClick={() => {
                const q = quick === "All" ? query : query.trim() ? `${quick} ${query}` : quick;
                void runSearch(q, nextCursor);
              }}
              className="px-4"
            >
              Load more
            </Button>
          </div>
        )}
      </div>
    </div>
  );
}
