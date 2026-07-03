import { type ReactNode, useState } from "react";
import { BadgeCheck, Check, Search } from "lucide-react";
import { Card } from "@/components/common/Card";
import { BackButton } from "@/components/common/DetailHeader";
import { Chip, Pill, StatusDot } from "@/components/common/bits";
import { REGISTRY, REGISTRY_CATS } from "@/fixtures";
import { useFixtures } from "@/store-fixtures";
import { useNav } from "@/store-nav";

export function RegistryView() {
  const nav = useNav();
  const { apps, registryState, installRegistry } = useFixtures();
  const [query, setQuery] = useState("");
  const [cat, setCat] = useState("All");

  const q = query.trim().toLowerCase();
  const items = REGISTRY.filter(
    (rg) => (cat === "All" || rg.cat === cat) && (q === "" || rg.name.toLowerCase().includes(q) || rg.desc.toLowerCase().includes(q)),
  );

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[860px]">
        <BackButton label="Apps" onClick={() => nav.navigate({ kind: "apps" })} />

        <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Registry</h2>
        <p className="m-0 mb-4 text-[13px] text-muted-foreground">
          MCP servers curated for Cockpit. Installs run on the workspace gateway you choose afterwards.
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
          {REGISTRY_CATS.map((c) => {
            const sel = c === cat;
            return (
              <button
                key={c}
                type="button"
                onClick={() => setCat(c)}
                className={`h-[26px] cursor-pointer rounded-full border px-3 font-sans text-xs font-medium ${
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

        {items.length === 0 && <div className="py-8 text-[13px] text-muted-foreground">No results found.</div>}

        <div className="grid grid-cols-2 gap-3">
          {items.map((rg) => {
            const state = registryState[rg.id];
            const installed = state === "installed" || apps.some((a) => a.id === rg.id);
            let action: ReactNode;
            if (installed) {
              action = (
                <span className="flex h-[27px] items-center gap-1.5 px-[11px] text-xs font-medium" style={{ color: "#22C55E" }}>
                  <Check aria-hidden size={13} strokeWidth={2.5} />
                  Installed
                </span>
              );
            } else if (state === "installing") {
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
                  onClick={() => installRegistry(rg.id)}
                  className="h-[27px] cursor-pointer rounded-md border-none bg-primary px-[13px] font-sans text-xs font-medium text-primary-foreground hover:opacity-85"
                >
                  Install
                </button>
              );
            }
            return (
              <Card key={rg.id} className="flex flex-col gap-3 px-[18px] py-4">
                <div className="flex items-center gap-3">
                  <Chip initial={rg.initial} color={rg.color} size={38} mono />
                  <div className="min-w-0 flex-1">
                    <div className="overflow-hidden text-ellipsis whitespace-nowrap text-sm font-semibold">{rg.name}</div>
                    <div className="flex items-center gap-[5px] text-[11.5px] text-muted-foreground">
                      {rg.publisher}
                      {rg.verified && (
                        <BadgeCheck aria-hidden size={12} strokeWidth={2} className="shrink-0" style={{ color: "#3B82F6" }} />
                      )}
                    </div>
                  </div>
                  <span className="shrink-0 text-[11px] text-muted-foreground">{rg.installs} installs</span>
                </div>
                <p className="m-0 text-[12.5px] leading-[1.5] text-muted-foreground">{rg.desc}</p>
                <div className="flex items-center gap-2 pt-0.5">
                  <Pill variant="mono">{rg.cat}</Pill>
                  <span className="flex-1" />
                  {action}
                </div>
              </Card>
            );
          })}
        </div>
      </div>
    </div>
  );
}
