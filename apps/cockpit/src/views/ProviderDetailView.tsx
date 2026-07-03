import { useEffect, useState } from "react";
import { ChevronDown, ChevronUp, Plus, Trash2 } from "lucide-react";
import { quotaColor, ROTATION_STRATEGIES, type RotationStrategy } from "@/constants";
import { providerById, useProviders } from "@/store-providers";
import { useNav } from "@/store-nav";
import type { AccountInfo } from "@/bindings";
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

function AccountRow({
  providerId,
  account,
  index,
  count,
  tracksUsage,
}: {
  providerId: string;
  account: AccountInfo;
  index: number;
  count: number;
  tracksUsage: boolean;
}) {
  const { moveAccount, setActiveAccount, removeAccount } = useProviders();

  return (
    <div className="flex items-start gap-3.5 border-b border-border px-[18px] py-3.5 last:border-b-0">
      <div className="flex shrink-0 flex-col items-center gap-px">
        <button
          type="button"
          title="Move up"
          onClick={() => void moveAccount(providerId, account.id, -1)}
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
          onClick={() => void moveAccount(providerId, account.id, 1)}
          className={`${moveBtn} ${index === count - 1 ? "invisible" : ""}`}
        >
          <ChevronDown aria-hidden size={11} strokeWidth={2.5} />
        </button>
      </div>
      <div className="min-w-0 flex-1">
        <div className="flex flex-wrap items-center gap-2">
          <span className="text-[13.5px] font-semibold">{account.label}</span>
          {account.active ? (
            <span
              className="rounded-full px-2 py-[2px] text-[10.5px] font-semibold tracking-[0.02em]"
              style={{ background: "color-mix(in oklab, #22C55E 18%, transparent)", color: "#22C55E" }}
            >
              Active
            </span>
          ) : (
            <Pill>Standby</Pill>
          )}
          {account.plan && <Pill variant="mono">{account.plan}</Pill>}
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
          {account.quotas.length === 0 && (
            <div className="text-[11.5px] text-muted-foreground">
              {account.active && tracksUsage
                ? "No limits set — edit the account to add session/weekly limits."
                : tracksUsage
                  ? "Usage is tracked against the active account."
                  : "No local usage data for this provider."}
            </div>
          )}
        </div>
      </div>
      {!account.active && (
        <button
          type="button"
          onClick={() => void setActiveAccount(providerId, account.id)}
          className="h-[27px] shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-[11px] font-sans text-xs font-medium text-foreground hover:bg-accent"
        >
          Set active
        </button>
      )}
      <button
        type="button"
        title="Remove account"
        onClick={() => void removeAccount(account.id)}
        className="flex h-[27px] w-[27px] shrink-0 cursor-pointer items-center justify-center rounded-md border border-border bg-transparent text-muted-foreground hover:bg-accent hover:text-destructive"
      >
        <Trash2 aria-hidden size={12} strokeWidth={2} />
      </button>
    </div>
  );
}

export function ProviderDetailView({ id }: { id: string }) {
  const nav = useNav();
  const { providers, loaded, hydrate, update, remove } = useProviders();
  const [addOpen, setAddOpen] = useState(false);

  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

  const provider = providerById(providers, id);
  if (!provider) {
    return <div className="flex flex-1 items-center justify-center text-[13px] text-muted-foreground">Unknown provider.</div>;
  }

  const count = provider.accounts.length;
  const usageTotal = provider.usage.reduce((sum, d) => sum + d.tok, 0);
  const usageData = provider.usage.map((u) => ({ day: u.day, tok: u.tok / 1_000_000 }));

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[860px]">
        <BackButton label="Providers" onClick={() => nav.navigate({ kind: "providers" })} />

        <DetailHeader
          chip={<Chip initial={provider.initial} color={provider.color} size={44} />}
          title={provider.name}
          sub={`${provider.kind} · ${count > 0 ? `${count} account${count === 1 ? "" : "s"}` : "No accounts"}`}
        >
          <button
            type="button"
            onClick={() => setAddOpen(true)}
            className="flex h-8 shrink-0 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-3 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent"
          >
            <Plus aria-hidden size={13} strokeWidth={2} />
            Add account
          </button>
          {provider.id !== "anthropic" && (
            <button
              type="button"
              title="Remove provider"
              onClick={() => {
                void remove(provider.id);
                nav.navigate({ kind: "providers" });
              }}
              className="flex h-8 w-8 shrink-0 cursor-pointer items-center justify-center rounded-md border border-border bg-transparent text-destructive hover:bg-accent"
            >
              <Trash2 aria-hidden size={13} strokeWidth={2} />
            </button>
          )}
          <Switch on={provider.enabled} onToggle={() => void update(id, { enabled: !provider.enabled })} label="Enabled" />
        </DetailHeader>

        {count === 0 ? (
          <Card className="p-6 text-[13px] text-muted-foreground">
            No accounts recorded. Add one to configure rotation and track usage against its limits.
          </Card>
        ) : (
          <>
            <Card>
              <CardHeader>
                <CardTitle>Accounts</CardTitle>
                <CardHint>Priority order — the top account serves requests first</CardHint>
              </CardHeader>
              {provider.accounts.map((ac, i) => (
                <AccountRow
                  key={ac.id}
                  providerId={id}
                  account={ac}
                  index={i}
                  count={provider.accounts.length}
                  tracksUsage={provider.tracksUsage}
                />
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
                      <Switch on={provider.failAuto} onToggle={() => void update(id, { failAuto: !provider.failAuto })} label="Auto-switch accounts" />
                    </CardRow>
                    <div className="flex flex-col border-b border-border px-[18px] pb-3 pt-2.5 last:border-b-0">
                      <span className="pb-1.5 pt-0.5 text-[13px] font-medium">Rotation strategy</span>
                      {ROTATION_STRATEGIES.map((stg) => {
                        const selected = provider.strategy === stg.id;
                        return (
                          <button
                            key={stg.id}
                            type="button"
                            onClick={() => void update(id, { strategy: stg.id as RotationStrategy })}
                            className="-mx-2.5 flex cursor-pointer items-start gap-2.5 rounded-md border-none bg-transparent px-2.5 py-[7px] text-left font-sans hover:bg-accent"
                          >
                            <span
                              className="mt-px flex h-[15px] w-[15px] flex-none items-center justify-center rounded-full border-[1.5px]"
                              style={{ borderColor: selected ? "var(--primary)" : "var(--border)" }}
                            >
                              <span className={`h-[7px] w-[7px] rounded-full bg-primary ${selected ? "opacity-100" : "opacity-0"}`} />
                            </span>
                            <span className="min-w-0 flex-1">
                              <span className="block text-[12.5px] font-medium text-foreground">{stg.label}</span>
                              <span className="mt-px block text-[11.5px] text-muted-foreground">{stg.desc}</span>
                            </span>
                          </button>
                        );
                      })}
                    </div>
                    {provider.strategy === "priority" && (
                      <>
                        <CardRow>
                          <span className="flex-1 text-[13px] font-medium">Switch when quota hits</span>
                          <Segmented
                            options={THRESHOLDS}
                            value={String(provider.threshold)}
                            onChange={(v) => void update(id, { threshold: Number(v) })}
                          />
                        </CardRow>
                        <CardRow>
                          <div className="min-w-0 flex-1">
                            <div className="text-[13px] font-medium">Return to primary</div>
                            <div className="mt-px text-[11.5px] text-muted-foreground">Switch back once the primary quota resets.</div>
                          </div>
                          <Switch
                            on={provider.returnToPrimary}
                            onToggle={() => void update(id, { returnToPrimary: !provider.returnToPrimary })}
                            label="Return to primary"
                          />
                        </CardRow>
                      </>
                    )}
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
                  <CardHint>estimated from local transcripts</CardHint>
                  <span className="flex-1" />
                  <span className="font-mono text-[11px] text-muted-foreground">≈{(usageTotal / 1_000_000).toFixed(1)}M tok this week</span>
                </CardHeader>
                {usageData.length > 0 ? (
                  <BarChart data={usageData} color={provider.color} />
                ) : (
                  <div className="px-[18px] py-4 text-[12.5px] text-muted-foreground">No local usage data for this provider.</div>
                )}
              </Card>
            </div>
          </>
        )}
      </div>
      <AddProviderModal open={addOpen} onClose={() => setAddOpen(false)} forProviderId={id} />
    </div>
  );
}
