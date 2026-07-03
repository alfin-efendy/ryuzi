import { useState } from "react";
import { ChevronDown, ChevronRight, Hourglass, Plus, RefreshCw } from "lucide-react";
import { PROVIDERS, ROTATION_STRATEGIES, quotaColor, type ProviderFixture } from "@/fixtures";
import { useFixtures } from "@/store-fixtures";
import { useNav } from "@/store-nav";
import { Card } from "@/components/common/Card";
import { Chip, Pill, QuotaTrack, StatusDot } from "@/components/common/bits";
import { Switch } from "@/components/common/Switch";
import { AddProviderModal } from "@/components/modals/AddProviderModal";

const filterBtn =
  "flex h-8 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-[color-mix(in_oklab,var(--card)_80%,transparent)] px-3 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent";
const iconBtn =
  "flex h-7 w-7 cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent";

function ProviderCard({ provider }: { provider: ProviderFixture }) {
  const nav = useNav();
  const state = useFixtures((s) => s.providerState[provider.id]);
  const toggleProvider = useFixtures((s) => s.toggleProvider);
  const open = () => nav.navigate({ kind: "providerDetail", id: provider.id });

  const count = provider.accounts.length;
  const active = provider.accounts.find((a) => a.id === state.activeAccount) ?? provider.accounts[0];
  const subtitle = count > 0 ? `${provider.kind} · ${count} account${count === 1 ? "" : "s"}` : provider.kind;

  return (
    <Card>
      <div className="flex items-center gap-3 border-b border-border px-[18px] py-3.5">
        <Chip initial={provider.initial} color={provider.color} size={34} onClick={open} />
        <button type="button" onClick={open} className="min-w-0 flex-1 cursor-pointer border-none bg-transparent p-0 text-left font-sans">
          <span className="flex items-center gap-2 text-sm font-semibold text-foreground">
            {provider.name}
            {state.failAuto && <Pill>{ROTATION_STRATEGIES.find((s) => s.id === state.strategy)?.pill ?? "Auto-failover"}</Pill>}
          </span>
          <span className="block text-xs text-muted-foreground">{subtitle}</span>
        </button>
        <button type="button" title="Refresh" className={iconBtn}>
          <RefreshCw aria-hidden size={13} strokeWidth={2} />
        </button>
        <Switch on={state.on} onToggle={() => toggleProvider(provider.id)} label="Enabled" />
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
            {active.label} · {active.plan} — active
          </div>
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
        </button>
      ) : (
        <button
          type="button"
          onClick={open}
          className="block w-full cursor-pointer border-none bg-transparent px-[18px] py-3 text-left font-sans text-[12.5px] text-muted-foreground"
        >
          No quota — local runtime.
        </button>
      )}
    </Card>
  );
}

export function ProvidersView() {
  const [addOpen, setAddOpen] = useState(false);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Providers</h2>
        <p className="m-0 mb-5 text-[13px] text-muted-foreground">Accounts and usage quotas across model providers.</p>

        <div className="mb-4 flex items-center gap-2">
          <button type="button" className={filterBtn}>
            All providers
            <ChevronDown aria-hidden size={12} strokeWidth={2} />
          </button>
          <button type="button" className={filterBtn}>
            All accounts
            <ChevronDown aria-hidden size={12} strokeWidth={2} />
          </button>
          <button type="button" className={filterBtn}>
            <Hourglass aria-hidden size={12} strokeWidth={2} />
            Expiring first
          </button>
          <div className="flex-1" />
          <button
            type="button"
            title="Refresh all"
            className="flex h-8 w-8 cursor-pointer items-center justify-center rounded-md border border-border bg-[color-mix(in_oklab,var(--card)_80%,transparent)] text-foreground hover:bg-accent"
          >
            <RefreshCw aria-hidden size={14} strokeWidth={2} />
          </button>
        </div>

        <div className="flex flex-col gap-3">
          {PROVIDERS.map((p) => (
            <ProviderCard key={p.id} provider={p} />
          ))}
          <button
            type="button"
            onClick={() => setAddOpen(true)}
            className="flex cursor-pointer items-center gap-3 rounded-xl border border-dashed border-border bg-transparent px-[18px] py-[15px] font-sans text-muted-foreground hover:bg-accent hover:text-accent-foreground"
          >
            <Plus aria-hidden size={16} strokeWidth={2} />
            <span className="text-[13px] font-medium">Add a provider — OAuth sign-in or API key</span>
          </button>
        </div>
      </div>
      <AddProviderModal open={addOpen} onClose={() => setAddOpen(false)} />
    </div>
  );
}
