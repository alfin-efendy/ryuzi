import { useEffect, useState } from "react";
import { ChevronRight, Plus, RefreshCw } from "lucide-react";
import { quotaColor, ROTATION_STRATEGIES } from "@/constants";
import { useProviders } from "@/store-providers";
import { useNav } from "@/store-nav";
import type { ProviderInfo } from "@/bindings";
import { Card } from "@/components/common/Card";
import { Chip, Pill, QuotaTrack, StatusDot } from "@/components/common/bits";
import { Switch } from "@/components/common/Switch";
import { AddProviderModal } from "@/components/modals/AddProviderModal";

const iconBtn =
  "flex h-7 w-7 cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent";

function ProviderCard({ provider }: { provider: ProviderInfo }) {
  const nav = useNav();
  const update = useProviders((s) => s.update);
  const open = () => nav.navigate({ kind: "providerDetail", id: provider.id });

  const count = provider.accounts.length;
  const active = provider.accounts.find((a) => a.active) ?? provider.accounts[0];
  const subtitle = count > 0 ? `${provider.kind} · ${count} account${count === 1 ? "" : "s"}` : provider.kind;

  return (
    <Card>
      <div className="flex items-center gap-3 border-b border-border px-[18px] py-3.5">
        <Chip initial={provider.initial} color={provider.color} size={34} onClick={open} />
        <button type="button" onClick={open} className="min-w-0 flex-1 cursor-pointer border-none bg-transparent p-0 text-left font-sans">
          <span className="flex items-center gap-2 text-sm font-semibold text-foreground">
            {provider.name}
            {provider.failAuto && <Pill>{ROTATION_STRATEGIES.find((s) => s.id === provider.strategy)?.pill ?? "Auto-failover"}</Pill>}
          </span>
          <span className="block text-xs text-muted-foreground">{subtitle}</span>
        </button>
        <Switch on={provider.enabled} onToggle={() => void update(provider.id, { enabled: !provider.enabled })} label="Enabled" />
        <button type="button" title="Details" onClick={open} className={`${iconBtn} hover:text-accent-foreground`}>
          <ChevronRight aria-hidden size={14} strokeWidth={2} />
        </button>
      </div>
      {active ? (
        <button
          type="button"
          onClick={open}
          className="block w-full cursor-pointer border-none bg-transparent px-[18px] pb-3.5 pt-2.5 text-left font-sans"
        >
          <div className="mb-2 text-[11px] text-muted-foreground">
            {active.label}
            {active.plan ? ` · ${active.plan}` : ""} — active
          </div>
          {active.quotas.length > 0 ? (
            <div className="flex flex-col gap-2.5">
              {active.quotas.map((q) => {
                const color = quotaColor(q.pct);
                return (
                  <div key={q.label} className="grid grid-cols-[150px_1fr_52px_130px] items-center gap-3.5">
                    <span className="flex items-center gap-[7px] text-[12.5px] font-semibold text-foreground">
                      <StatusDot color={color} />
                      {q.label}
                    </span>
                    <span className="flex flex-col gap-[3px]">
                      <QuotaTrack pct={q.pct} color={color} />
                      <span className="font-mono text-[10.5px] text-muted-foreground">
                        {q.used} / {q.max}
                      </span>
                    </span>
                    <span className="text-right font-mono text-xs" style={{ color }}>
                      {q.pct}%
                    </span>
                    <span className="text-xs text-muted-foreground">{q.resets}</span>
                  </div>
                );
              })}
            </div>
          ) : (
            <div className="text-[12.5px] text-muted-foreground">
              {provider.tracksUsage
                ? "No limits set — add session/weekly limits on the account to track quotas."
                : "No local usage data for this provider yet."}
            </div>
          )}
        </button>
      ) : (
        <button
          type="button"
          onClick={open}
          className="block w-full cursor-pointer border-none bg-transparent px-[18px] py-3 text-left font-sans text-[12.5px] text-muted-foreground"
        >
          No accounts recorded yet.
        </button>
      )}
    </Card>
  );
}

export function ProvidersView() {
  const { providers, loaded, hydrate } = useProviders();
  const [addOpen, setAddOpen] = useState(false);

  useEffect(() => {
    void hydrate();
  }, [hydrate]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <div className="mb-5 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Providers</h2>
            <p className="m-0 text-[13px] text-muted-foreground">
              Accounts and locally-tracked usage across model providers. Estimated from your transcript history.
            </p>
          </div>
          <button
            type="button"
            title="Refresh"
            onClick={() => void hydrate()}
            className="flex h-8 w-8 cursor-pointer items-center justify-center rounded-md border border-border bg-[color-mix(in_oklab,var(--card)_80%,transparent)] text-foreground hover:bg-accent"
          >
            <RefreshCw aria-hidden size={14} strokeWidth={2} />
          </button>
        </div>

        <div className="flex flex-col gap-3">
          {providers.map((p) => (
            <ProviderCard key={p.id} provider={p} />
          ))}
          {loaded && (
            <button
              type="button"
              onClick={() => setAddOpen(true)}
              className="flex cursor-pointer items-center gap-3 rounded-xl border border-dashed border-border bg-transparent px-[18px] py-[15px] font-sans text-muted-foreground hover:bg-accent hover:text-accent-foreground"
            >
              <Plus aria-hidden size={16} strokeWidth={2} />
              <span className="text-[13px] font-medium">Add a provider — track its accounts and usage</span>
            </button>
          )}
        </div>
      </div>
      <AddProviderModal open={addOpen} onClose={() => setAddOpen(false)} />
    </div>
  );
}
