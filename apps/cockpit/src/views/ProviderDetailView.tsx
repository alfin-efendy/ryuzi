import { useState } from "react";
import { ChevronDown, ChevronUp, Plus } from "lucide-react";
import { PROVIDERS, quotaColor, type ProviderAccount } from "@/fixtures";
import { useFixtures } from "@/store-fixtures";
import { useNav } from "@/store-nav";
import { BarChart } from "@/components/common/BarChart";
import { Card, CardHeader, CardHint, CardRow, CardTitle } from "@/components/common/Card";
import { Chip, Pill, QuotaTrack } from "@/components/common/bits";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { Segmented } from "@/components/common/Segmented";
import { Switch } from "@/components/common/Switch";
import { AddProviderModal } from "@/components/modals/AddProviderModal";

const THRESHOLDS: { id: string; label: string }[] = [
  { id: "90", label: "90%" },
  { id: "95", label: "95%" },
  { id: "99", label: "99%" },
];

const moveBtn =
  "flex h-[15px] w-5 cursor-pointer items-center justify-center border-none bg-transparent p-0 text-muted-foreground hover:text-foreground";

function AccountRow({ providerId, account, index, count }: { providerId: string; account: ProviderAccount; index: number; count: number }) {
  const fx = useFixtures();
  const isActive = fx.providerState[providerId].activeAccount === account.id;

  return (
    <div className="flex items-start gap-3.5 border-b border-border px-[18px] py-3.5 last:border-b-0">
      <div className="flex shrink-0 flex-col items-center gap-px">
        <button
          type="button"
          title="Move up"
          onClick={() => fx.moveAccount(providerId, account.id, -1)}
          className={`${moveBtn} ${index === 0 ? "invisible" : ""}`}
        >
          <ChevronUp aria-hidden size={11} strokeWidth={2.5} />
        </button>
        <span className="flex h-5 w-5 items-center justify-center rounded-full bg-muted font-mono text-[10.5px] font-semibold text-muted-foreground">
          {index + 1}
        </span>
        <button
          type="button"
          title="Move down"
          onClick={() => fx.moveAccount(providerId, account.id, 1)}
          className={`${moveBtn} ${index === count - 1 ? "invisible" : ""}`}
        >
          <ChevronDown aria-hidden size={11} strokeWidth={2.5} />
        </button>
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-[13.5px] font-semibold">{account.label}</span>
          {isActive ? (
            <span
              className="rounded-full px-2 py-[2px] text-[10.5px] font-semibold tracking-[0.02em]"
              style={{ background: "color-mix(in oklab, #22C55E 18%, transparent)", color: "#22C55E" }}
            >
              Active
            </span>
          ) : (
            <Pill>Standby</Pill>
          )}
          <Pill variant="mono">{account.plan}</Pill>
          <span className="text-xs text-muted-foreground">{account.email}</span>
        </div>
        <div className="mt-2.5 flex flex-col gap-2">
          {account.quotas.map((q) => {
            const color = quotaColor(q.pct);
            return (
              <div key={q.label} className="grid grid-cols-[110px_1fr_46px_150px] items-center gap-3.5">
                <span className="text-xs font-medium text-muted-foreground">{q.label}</span>
                <span className="flex flex-col gap-[3px]">
                  <QuotaTrack pct={q.pct} color={color} />
                  <span className="font-mono text-[10.5px] text-muted-foreground">
                    {q.used} / {q.max}
                  </span>
                </span>
                <span className="text-right font-mono text-xs" style={{ color }}>
                  {q.pct}%
                </span>
                <span className="text-[11.5px] text-muted-foreground">{q.resets}</span>
              </div>
            );
          })}
        </div>
      </div>
      {!isActive && (
        <button
          type="button"
          onClick={() => fx.setActiveAccount(providerId, account.id)}
          className="h-[27px] shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-[11px] font-sans text-xs font-medium text-foreground hover:bg-accent"
        >
          Set active
        </button>
      )}
    </div>
  );
}

export function ProviderDetailView({ id }: { id: string }) {
  const nav = useNav();
  const fx = useFixtures();
  const [addOpen, setAddOpen] = useState(false);

  const provider = PROVIDERS.find((p) => p.id === id);
  const state = fx.providerState[id];
  if (!provider || !state) {
    return <div className="flex flex-1 items-center justify-center text-[13px] text-muted-foreground">Unknown provider.</div>;
  }

  const ordered = state.accountOrder
    .map((aid) => provider.accounts.find((a) => a.id === aid))
    .filter((a): a is ProviderAccount => a !== undefined);
  const count = provider.accounts.length;
  const canAddAccount = provider.kind.includes("OAuth");
  const usageTotal = provider.usage.reduce((sum, d) => sum + d.tok, 0);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[860px]">
        <BackButton label="Providers" onClick={() => nav.navigate({ kind: "providers" })} />

        <DetailHeader
          chip={<Chip initial={provider.initial} color={provider.color} size={44} />}
          title={provider.name}
          sub={`${provider.kind} · ${count > 0 ? `${count} account${count === 1 ? "" : "s"}` : "No accounts"}`}
        >
          {canAddAccount && (
            <button
              type="button"
              onClick={() => setAddOpen(true)}
              className="flex h-8 shrink-0 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-3 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent"
            >
              <Plus aria-hidden size={13} strokeWidth={2} />
              Add account
            </button>
          )}
          <Switch on={state.on} onToggle={() => fx.toggleProvider(id)} label="Enabled" />
        </DetailHeader>

        {count === 0 ? (
          <Card className="p-6 text-[13px] text-muted-foreground">Local runtime — no accounts, quotas or usage metering.</Card>
        ) : (
          <>
            <Card>
              <CardHeader>
                <CardTitle>Accounts</CardTitle>
                <CardHint>Priority order — the top account serves requests first</CardHint>
              </CardHeader>
              {ordered.map((ac, i) => (
                <AccountRow key={ac.id} providerId={id} account={ac} index={i} count={ordered.length} />
              ))}
            </Card>

            <div className="mt-3 grid grid-cols-2 items-start gap-3">
              <Card>
                <CardHeader>
                  <CardTitle>Failover</CardTitle>
                </CardHeader>
                {count > 1 ? (
                  <>
                    <CardRow>
                      <div className="min-w-0 flex-1">
                        <div className="text-[13px] font-medium">Auto-switch accounts</div>
                        <div className="mt-px text-[11.5px] text-muted-foreground">Rotate to the next account when a quota runs out.</div>
                      </div>
                      <Switch on={state.failAuto} onToggle={() => fx.setFailAuto(id, !state.failAuto)} label="Auto-switch accounts" />
                    </CardRow>
                    <CardRow>
                      <span className="flex-1 text-[13px] font-medium">Switch when quota hits</span>
                      <Segmented options={THRESHOLDS} value={String(state.threshold)} onChange={(v) => fx.setThreshold(id, Number(v))} />
                    </CardRow>
                    <CardRow>
                      <div className="min-w-0 flex-1">
                        <div className="text-[13px] font-medium">Return to primary</div>
                        <div className="mt-px text-[11.5px] text-muted-foreground">Switch back once the primary quota resets.</div>
                      </div>
                      <Switch
                        on={state.returnToPrimary}
                        onToggle={() => fx.setReturnToPrimary(id, !state.returnToPrimary)}
                        label="Return to primary"
                      />
                    </CardRow>
                  </>
                ) : (
                  <div className="px-[18px] py-4 text-[12.5px] text-muted-foreground">
                    Add a second account to enable automatic rotation when this quota runs out.
                  </div>
                )}
              </Card>

              <Card>
                <CardHeader>
                  <CardTitle>Usage</CardTitle>
                  <span className="flex-1" />
                  <span className="font-mono text-[11px] text-muted-foreground">{usageTotal.toFixed(1)}M tok this week</span>
                </CardHeader>
                {provider.usage.length > 0 && <BarChart data={provider.usage} color={provider.color} />}
              </Card>
            </div>
          </>
        )}
      </div>
      <AddProviderModal open={addOpen} onClose={() => setAddOpen(false)} />
    </div>
  );
}
