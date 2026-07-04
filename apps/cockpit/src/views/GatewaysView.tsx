import { ChevronRight, Plus, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { quotaColor } from "@/constants";
import { formatLastSeen, useGateways } from "@/store-gateways";
import { useNav } from "@/store-nav";
import { useStore } from "@/store";
import type { GatewayInfo } from "@/bindings";
import { Button, SettingsCard as Card } from "@ryuzi/ui";
import { AddGatewayModal } from "@/components/modals/AddGatewayModal";
import { QuotaTrack, StatusDot } from "@/components/common/bits";

function plural(n: number, word: string): string {
  return `${n} ${word}${n === 1 ? "" : "s"}`;
}

function GatewayCard({ g }: { g: GatewayInfo }) {
  const nav = useNav();
  // Real sessions all run on the local gateway until the remote daemon ships.
  const sessionCount = useStore((s) => (g.id === "local" ? s.sessions.length : 0));

  const online = g.status === "connected";
  const statusColor = online ? "#22C55E" : "var(--muted-foreground)";

  return (
    <Card>
      <Button
        variant="ghost"
        onClick={() => nav.navigate({ kind: "gatewayDetail", id: g.id })}
        className="h-auto w-full justify-start gap-3 whitespace-normal rounded-none px-[18px] py-3.5 text-left font-normal"
      >
        <span className="flex h-[38px] w-[38px] shrink-0 items-center justify-center rounded-md bg-muted font-mono text-[10.5px] font-semibold text-muted-foreground">
          {g.badge}
        </span>
        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-2">
            <span className="text-sm font-semibold text-foreground">{g.name}</span>
          </span>
          <span className="mt-0.5 block text-xs text-muted-foreground">{g.metaLine}</span>
          <span className="mt-0.5 block text-xs text-muted-foreground">
            {g.id === "local" ? plural(sessionCount, "session") : `daemon ${g.daemonVersion}`}
          </span>
        </span>
        <span className="flex shrink-0 items-center gap-1.5 text-xs" style={{ color: statusColor }}>
          <StatusDot color={statusColor} size={7} />
          {online ? "Connected" : "Offline"}
        </span>
        <ChevronRight aria-hidden size={14} strokeWidth={2} className="size-3.5 shrink-0 text-muted-foreground" />
      </Button>
      {online && g.resources.length > 0 && (
        <div className="grid grid-cols-3 gap-[18px] px-[18px] pb-3.5">
          {g.resources.map((r) => (
            <div key={r.label} className="flex flex-col gap-1">
              <div className="flex items-baseline gap-2">
                <span className="text-[11px] font-medium text-muted-foreground">{r.label}</span>
                <span className="flex-1" />
                <span className="font-mono text-[10.5px] text-muted-foreground">{r.pct}%</span>
              </div>
              <QuotaTrack pct={r.pct} color={quotaColor(r.pct)} />
            </div>
          ))}
        </div>
      )}
      {!online && <div className="px-[18px] pb-3.5 text-xs text-muted-foreground">Offline — last seen {formatLastSeen(g.lastSeenMs)}.</div>}
    </Card>
  );
}

export function GatewaysView() {
  const { gateways, loaded, probing, hydrate, probe } = useGateways();
  const [addOpen, setAddOpen] = useState(false);

  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <div className="mb-5 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Gateways</h2>
            <p className="m-0 text-[13px] text-muted-foreground">
              Machines that run your projects, sessions, agents and apps. Cockpit talks to a daemon on each one.
            </p>
          </div>
          <Button variant="outline" onClick={() => void probe()} disabled={probing} className="shrink-0">
            <RefreshCw aria-hidden size={13} strokeWidth={2} className={probing ? "size-[13px] animate-spin" : "size-[13px]"} />
            {probing ? "Probing…" : "Probe"}
          </Button>
          <Button onClick={() => setAddOpen(true)} className="shrink-0">
            <Plus aria-hidden size={14} strokeWidth={2} className="size-3.5" />
            Connect gateway
          </Button>
        </div>

        <div className="flex flex-col gap-3">
          {gateways.map((g) => (
            <GatewayCard key={g.id} g={g} />
          ))}
          {loaded && gateways.length === 0 && <div className="py-8 text-center text-[13px] text-muted-foreground">Detecting gateways…</div>}
        </div>
      </div>
      {addOpen && <AddGatewayModal onClose={() => setAddOpen(false)} />}
    </div>
  );
}
