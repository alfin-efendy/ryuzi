import { Plus, RefreshCw, Trash2, TriangleAlert } from "lucide-react";
import { useEffect } from "react";
import { toast } from "sonner";
import { GW_FS_MODES, quotaColor, type GatewayFsMode } from "@/constants";
import { eventColor, formatLastSeen, gatewayById, useGateways } from "@/store-gateways";
import { useNav } from "@/store-nav";
import { useStore } from "@/store";
import { commands } from "@/bindings";
import { statusMeta } from "@/lib/status";
import { sessionTitle } from "@/lib/sidebar";
import { refOf } from "@/lib/session-key";
import {
  Button,
  Segmented,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardTitle as CardTitle,
} from "@ryuzi/ui";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { QuotaTrack, StatusDot } from "@/components/common/bits";

const sectionLabel = "px-[18px] pb-1 pt-2.5 text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground";

function HealthRow({ label, value }: { label: string; value: string }) {
  return (
    <div className="flex items-center gap-2 border-b border-border px-[18px] py-2.5 last:border-b-0">
      <span className="flex-1 text-[12.5px] text-muted-foreground">{label}</span>
      <span className="font-mono text-xs">{value}</span>
    </div>
  );
}

export function GatewayDetailView({ id }: { id: string }) {
  const nav = useNav();
  const { gateways, eventsById, probing, probe, remove, updateFs, loadEvents } = useGateways();
  const sessions = useStore((s) => s.sessions);
  const setFocused = useStore((s) => s.setFocused);

  const g = gatewayById(gateways, id);
  const events = eventsById[id] ?? [];

  useEffect(() => {
    void loadEvents(id);
  }, [id, loadEvents]);

  if (!g) {
    return <div className="flex flex-1 items-center justify-center text-[13px] text-muted-foreground">Gateway not found.</div>;
  }

  const online = g.status === "connected";
  const statusColor = online ? "#22C55E" : "var(--muted-foreground)";
  const fsDesc = GW_FS_MODES.find((m) => m.id === g.fsMode)?.desc;

  // Sessions are stamped with the runner (gateway) that owns them — this gateway's
  // route id IS a runner id (LOCAL_RUNNER for the local one, gateway.id for a remote).
  const gwSessions = sessions.filter((s) => s.runnerId === id && s.status !== "ended");

  const addFolder = async () => {
    const dir = await commands.pickDirectory();
    if (dir && !g.paths.includes(dir)) void updateFs(g.id, g.fsMode, [...g.paths, dir]);
  };

  const copyLog = () => {
    const text = events.map((e) => `[${new Date(e.at).toLocaleTimeString()}] ${e.text}`).join("\n");
    void navigator.clipboard.writeText(text);
    toast.success("Log copied");
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[760px]">
        <BackButton label="Gateways" onClick={() => nav.navigate({ kind: "gateways" })} />

        <DetailHeader
          chip={
            <span className="flex h-11 w-11 shrink-0 items-center justify-center rounded-lg bg-muted font-mono text-xs font-semibold text-muted-foreground">
              {g.badge}
            </span>
          }
          title={g.name}
          sub={g.detail}
        >
          <span className="flex shrink-0 items-center gap-1.5 text-xs" style={{ color: statusColor }}>
            <StatusDot color={statusColor} size={8} pulse={online} />
            {online ? "Connected" : "Offline"}
          </span>
          <Button variant="outline" onClick={() => void probe()} disabled={probing} className="shrink-0">
            <RefreshCw aria-hidden size={13} strokeWidth={2} className={probing ? "size-[13px] animate-spin" : "size-[13px]"} />
            {probing ? "Probing…" : "Probe now"}
          </Button>
          {(g.kind === "ssh" || g.kind === "remote") && (
            <Button
              variant="outline"
              size="icon"
              onClick={() => {
                void remove(g.id);
                nav.navigate({ kind: "gateways" });
              }}
              title={g.kind === "remote" ? "Remove runner" : "Remove gateway"}
              className="shrink-0 text-destructive hover:text-destructive"
            >
              <Trash2 aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
            </Button>
          )}
        </DetailHeader>

        {!online && (
          <Card className="mb-3">
            <div className="flex items-start gap-3 px-[18px] py-3.5">
              <TriangleAlert aria-hidden size={16} strokeWidth={2} className="mt-px shrink-0" style={{ color: "#EF4444" }} />
              <div className="min-w-0 flex-1">
                <div className="text-[13.5px] font-semibold">Offline — last seen {formatLastSeen(g.lastSeenMs)}</div>
                <div className="mt-[3px] text-[12.5px] leading-[1.55] text-muted-foreground">
                  {g.kind === "ssh" || g.kind === "remote"
                    ? "The TCP probe couldn't reach the host. Check the address, port, and firewall, then probe again."
                    : "The distro isn't running. Start it and probe again."}
                </div>
              </div>
            </div>
          </Card>
        )}

        <div className="mb-3 grid grid-cols-2 items-start gap-3">
          <Card>
            <CardHeader>
              <CardTitle>Health</CardTitle>
            </CardHeader>
            <HealthRow label="Latency" value={g.latency ?? "—"} />
            <HealthRow label="Uptime" value={g.uptime ?? "—"} />
            <HealthRow label="Cockpit daemon" value={g.daemonVersion} />
            <HealthRow label="Last seen" value={formatLastSeen(g.lastSeenMs)} />
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Resources</CardTitle>
            </CardHeader>
            {online && g.resources.length > 0 ? (
              <div className="flex flex-col gap-3.5 px-[18px] py-3.5">
                {g.resources.map((r) => {
                  const color = quotaColor(r.pct);
                  return (
                    <div key={r.label} className="flex flex-col gap-[5px]">
                      <div className="flex items-baseline gap-2">
                        <span className="text-xs font-medium">{r.label}</span>
                        <span className="font-mono text-[10.5px] text-muted-foreground">{r.sub}</span>
                        <span className="flex-1" />
                        <span className="font-mono text-[11.5px]" style={{ color }}>
                          {r.pct}%
                        </span>
                      </div>
                      <QuotaTrack pct={r.pct} color={color} height={5} />
                    </div>
                  );
                })}
              </div>
            ) : (
              <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">
                {online ? "Telemetry arrives with the remote daemon." : "No telemetry while offline."}
              </div>
            )}
          </Card>
        </div>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Running on this gateway</CardTitle>
          </CardHeader>
          {gwSessions.length === 0 && (
            <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">
              {id === "local" ? "Nothing runs here yet. Start a session from Home." : "Remote sessions arrive with the gateway daemon."}
            </div>
          )}
          {gwSessions.length > 0 && (
            <>
              <div className={sectionLabel}>Sessions · {gwSessions.length}</div>
              {gwSessions.map((s) => {
                const m = statusMeta(s.status);
                return (
                  <Button
                    key={s.sessionPk}
                    variant="ghost"
                    onClick={() => {
                      setFocused(refOf(s));
                      nav.navigate({ kind: "session" });
                    }}
                    className="h-auto w-full justify-start gap-2.5 rounded-none px-[18px] py-2 text-left"
                  >
                    <StatusDot color={m.color} size={7} pulse={m.pulse} />
                    <span className="min-w-0 flex-1 truncate text-foreground">{sessionTitle(s)}</span>
                    <span className="shrink-0 text-xs font-normal text-muted-foreground">Claude Code</span>
                  </Button>
                );
              })}
            </>
          )}
        </Card>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Security</CardTitle>
          </CardHeader>
          <div className="flex items-center gap-2.5 border-b border-border px-[18px] py-[11px]">
            <span className="w-[120px] shrink-0 text-[12.5px] font-medium">Host fingerprint</span>
            {g.fingerprint ? (
              <>
                <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground">{g.fingerprint}</span>
                <Button
                  variant="outline"
                  size="xs"
                  onClick={() => g.fingerprint && void navigator.clipboard.writeText(g.fingerprint)}
                  className="shrink-0"
                >
                  Copy
                </Button>
              </>
            ) : (
              <span className="flex-1 text-xs text-muted-foreground">
                {g.kind === "ssh" ? "Recorded on first daemon handshake." : "Local machine — no host key needed."}
              </span>
            )}
          </div>
          <div className="flex flex-col gap-2 px-[18px] py-3">
            <div className="flex items-center gap-3">
              <span className="flex-1 text-[12.5px] font-medium">Filesystem access</span>
              <Segmented
                options={GW_FS_MODES.map((m) => ({ id: m.id, label: m.label }))}
                value={g.fsMode as GatewayFsMode}
                onChange={(m) => void updateFs(g.id, m, g.paths)}
              />
            </div>
            <div className="text-right text-[11.5px] text-muted-foreground">{fsDesc}</div>
            {g.fsMode === "projects" && (
              <div className="flex flex-wrap gap-1.5">
                {g.paths.map((p) => (
                  <Button
                    key={p}
                    variant="outline"
                    size="xs"
                    title="Remove folder"
                    onClick={() =>
                      void updateFs(
                        g.id,
                        g.fsMode,
                        g.paths.filter((x) => x !== p),
                      )
                    }
                    className="rounded-full font-mono"
                  >
                    {p}
                  </Button>
                ))}
                {g.kind === "local" && (
                  <Button
                    variant="outline"
                    size="xs"
                    onClick={() => void addFolder()}
                    className="rounded-full border-dashed text-muted-foreground"
                  >
                    <Plus aria-hidden size={10} strokeWidth={2} className="size-2.5" />
                    Add folder
                  </Button>
                )}
              </div>
            )}
          </div>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Log</CardTitle>
            <CardHint>Daemon events, most recent last</CardHint>
            <span className="flex-1" />
            <Button variant="outline" size="xs" onClick={copyLog} className="shrink-0">
              Copy
            </Button>
          </CardHeader>
          <div className="overflow-x-auto bg-code px-[18px] py-3 font-mono text-[11.5px] leading-[1.75] text-code-foreground">
            {events.length === 0 && <div className="text-muted-foreground">No events recorded yet.</div>}
            {events.map((e, i) => (
              <div key={`${e.at}-${i}`} className="whitespace-pre-wrap" style={{ color: eventColor(e.level) }}>
                [{new Date(e.at).toLocaleTimeString()}] {e.text}
              </div>
            ))}
          </div>
        </Card>
      </div>
    </div>
  );
}
