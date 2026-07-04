import { type ReactNode, useCallback, useEffect, useState } from "react";
import { Check, CircleAlert, Search } from "lucide-react";
import { toast } from "sonner";
import { commands, type RegistryEntry } from "@/bindings";
import { Card } from "@/components/common/Card";
import { BackButton } from "@/components/common/DetailHeader";
import { Chip, Pill, StatusDot } from "@/components/common/bits";
import { useApps } from "@/store-apps";
import { useNav } from "@/store-nav";

// Curated quick searches against the live registry (it has no category API).
const QUICK_SEARCHES = ["All", "github", "database", "browser", "docs", "monitoring", "payments", "deploy"];

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
    setEntries((prev) => (cursor ? [...prev, ...res.data.entries] : res.data.entries));
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
          <input
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            placeholder="Search the registry"
            aria-label="Search the registry"
            className="min-w-0 flex-1 border-none bg-transparent font-sans text-[13px] text-foreground outline-none"
          />
        </div>

        <div className="mb-4 flex flex-wrap gap-1.5">
          {QUICK_SEARCHES.map((c) => {
            const sel = c === quick;
            return (
              <button
                key={c}
                type="button"
                onClick={() => setQuick(c)}
                className={`h-[26px] cursor-pointer rounded-full border px-3 font-sans text-xs font-medium capitalize ${
                  sel
                    ? "border-transparent bg-primary text-primary-foreground"
                    : "border-border bg-transparent text-muted-foreground hover:bg-accent"
                }`}
              >
                {c}
              </button>
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
                <button
                  type="button"
                  onClick={() => void install(rg)}
                  disabled={!rg.installTarget}
                  className="h-[27px] cursor-pointer rounded-md border-none bg-primary px-[13px] font-sans text-xs font-medium text-primary-foreground hover:opacity-85 disabled:cursor-default disabled:opacity-45"
                >
                  Install
                </button>
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
            <button
              type="button"
              onClick={() => {
                const q = quick === "All" ? query : query.trim() ? `${quick} ${query}` : quick;
                void runSearch(q, nextCursor);
              }}
              className="h-8 cursor-pointer rounded-md border border-border bg-transparent px-4 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent"
            >
              Load more
            </button>
          </div>
        )}
      </div>
    </div>
  );
}
