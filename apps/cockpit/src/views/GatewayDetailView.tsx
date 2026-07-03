import type { CSSProperties, ReactNode } from "react";
import { ArrowLeftRight, Check, Clock, HardDriveDownload, Plus, RefreshCw, TriangleAlert } from "lucide-react";
import { GW_FS_MODES, quotaColor, WORKSPACES } from "@/fixtures";
import { useFixtures } from "@/store-fixtures";
import { useNav } from "@/store-nav";
import { useStore } from "@/store";
import { statusMeta } from "@/lib/status";
import { sessionTitle } from "@/lib/sidebar";
import { Card, CardHeader, CardHint, CardTitle } from "@/components/common/Card";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { Segmented } from "@/components/common/Segmented";
import { Chip, QuotaTrack, StatusDot } from "@/components/common/bits";

const smallBtn =
  "h-[26px] shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-2.5 font-sans text-[11.5px] font-medium text-foreground hover:bg-accent";
const runRow =
  "flex w-full cursor-pointer items-center gap-2.5 border-none bg-transparent px-[18px] py-2 text-left font-sans hover:bg-accent";
const sectionLabel = "px-[18px] pb-1 pt-2.5 text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground";
const warnPill: CSSProperties = { background: "color-mix(in oklab, #F59E0B 18%, transparent)", color: "#F59E0B" };

function HealthRow({ label, value, extra }: { label: string; value: string; extra?: ReactNode }) {
  return (
    <div className="flex items-center gap-2 border-b border-border px-[18px] py-2.5 last:border-b-0">
      <span className="flex-1 text-[12.5px] text-muted-foreground">{label}</span>
      <span className="font-mono text-xs">{value}</span>
      {extra}
    </div>
  );
}

export function GatewayDetailView({ id }: { id: string }) {
  const nav = useNav();
  const gw = useFixtures((s) => s.gatewayState[id]);
  const setGatewayFsMode = useFixtures((s) => s.setGatewayFsMode);
  const applyGatewayUpdate = useFixtures((s) => s.applyGatewayUpdate);
  const jobs = useFixtures((s) => s.jobs);
  const apps = useFixtures((s) => s.apps);
  const sessions = useStore((s) => s.sessions);
  const setFocused = useStore((s) => s.setFocused);

  const w = WORKSPACES.find((x) => x.id === id);
  if (!w || !gw) {
    return <div className="flex flex-1 items-center justify-center text-[13px] text-muted-foreground">Gateway not found.</div>;
  }

  const online = w.status === "connected";
  const hasUpdate = gw.daemon !== w.daemonLatest;
  const statusColor = online ? "#22C55E" : "var(--muted-foreground)";
  const fp = w.fingerprint;
  const fsDesc = GW_FS_MODES.find((m) => m.id === gw.fsMode)?.desc;

  // Real sessions all run on the local daemon today; remote gateways host none yet.
  const gwSessions = id === "local" ? sessions : [];
  const gwJobs = jobs.filter((j) => j.workspace === id);
  const gwApps = apps.filter((a) => a.scope === "global" || a.scopeWs[id]);
  const empty = gwSessions.length === 0 && gwJobs.length === 0 && gwApps.length === 0;

  const localName = WORKSPACES.find((x) => x.id === "local")?.name ?? "This PC";
  const queuedLabel =
    gwJobs.length === 0 ? "No scheduled runs queued." : `${gwJobs.length} scheduled run${gwJobs.length === 1 ? "" : "s"} queued.`;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[760px]">
        <BackButton label="Gateways" onClick={() => nav.navigate({ kind: "gateways" })} />

        <DetailHeader
          chip={
            <span className="flex h-11 w-11 shrink-0 items-center justify-center rounded-lg bg-muted font-mono text-xs font-semibold text-muted-foreground">
              {w.badge}
            </span>
          }
          title={w.name}
          sub={w.detail}
        >
          <span className="flex shrink-0 items-center gap-1.5 text-xs" style={{ color: statusColor }}>
            <StatusDot color={statusColor} size={8} pulse={online} />
            {online ? "Connected" : "Offline"}
          </span>
          {online ? (
            <button
              type="button"
              className="flex h-8 shrink-0 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-3 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent"
            >
              <RefreshCw aria-hidden size={13} strokeWidth={2} />
              Restart daemon
            </button>
          ) : (
            <button
              type="button"
              className="h-8 shrink-0 cursor-pointer rounded-md border-none bg-primary px-3.5 font-sans text-[12.5px] font-medium text-primary-foreground hover:opacity-85"
            >
              Retry now
            </button>
          )}
        </DetailHeader>

        {!online && (
          <Card className="mb-3">
            <div className="flex items-start gap-3 px-[18px] py-3.5">
              <TriangleAlert aria-hidden size={16} strokeWidth={2} className="mt-px shrink-0" style={{ color: "#EF4444" }} />
              <div className="min-w-0 flex-1">
                <div className="text-[13.5px] font-semibold">Offline — last seen {w.lastSeen}</div>
                <div className="mt-[3px] text-[12.5px] leading-[1.55] text-muted-foreground">
                  Retrying every 5m · attempt 24. {queuedLabel}
                </div>
                {gwJobs.length > 0 && (
                  <button
                    type="button"
                    className="mt-2.5 flex h-7 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-[11px] font-sans text-xs font-medium text-foreground hover:bg-accent"
                  >
                    <ArrowLeftRight aria-hidden size={12} strokeWidth={2} />
                    Move {gwSessions.length} session{gwSessions.length === 1 ? "" : "s"} to {localName}
                  </button>
                )}
              </div>
            </div>
          </Card>
        )}

        {hasUpdate && (
          <Card className="mb-3">
            <div className="flex items-center gap-3 px-[18px] py-3.5">
              <HardDriveDownload aria-hidden size={16} strokeWidth={2} className="shrink-0" style={{ color: "#F59E0B" }} />
              <div className="min-w-0 flex-1">
                <div className="text-[13.5px] font-semibold">Daemon update available — {w.daemonLatest}</div>
                <div className="mt-px text-xs text-muted-foreground">
                  Running {gw.daemon}. Updates restart the daemon; active sessions reconnect automatically.
                </div>
              </div>
              <button
                type="button"
                onClick={() => applyGatewayUpdate(id)}
                className="h-[30px] shrink-0 cursor-pointer rounded-md border-none bg-primary px-3.5 font-sans text-[12.5px] font-medium text-primary-foreground hover:opacity-85"
              >
                Update now
              </button>
            </div>
          </Card>
        )}

        <div className="mb-3 grid grid-cols-2 items-start gap-3">
          <Card>
            <CardHeader>
              <CardTitle>Health</CardTitle>
            </CardHeader>
            <HealthRow label="Latency" value={w.lat} />
            <HealthRow label="Uptime" value={w.uptime} />
            <HealthRow
              label="Cockpit daemon"
              value={gw.daemon}
              extra={
                hasUpdate && (
                  <span className="shrink-0 rounded-full px-1.5 py-px text-[10px] font-semibold" style={warnPill}>
                    {w.daemonLatest}
                  </span>
                )
              }
            />
            <HealthRow label="Last seen" value={w.lastSeen} />
          </Card>

          <Card>
            <CardHeader>
              <CardTitle>Resources</CardTitle>
            </CardHeader>
            {online ? (
              <div className="flex flex-col gap-3.5 px-[18px] py-3.5">
                {w.resources.map((r) => {
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
              <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">No telemetry while offline.</div>
            )}
          </Card>
        </div>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Running on this gateway</CardTitle>
          </CardHeader>
          {empty && (
            <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">
              Nothing runs here yet. Pick this gateway in the composer or a scheduled job.
            </div>
          )}
          {gwSessions.length > 0 && (
            <>
              <div className={sectionLabel}>Sessions · {gwSessions.length}</div>
              {gwSessions.map((s) => {
                const m = statusMeta(s.status);
                return (
                  <button
                    key={s.sessionPk}
                    type="button"
                    onClick={() => {
                      setFocused(s.sessionPk);
                      nav.navigate({ kind: "session" });
                    }}
                    className={runRow}
                  >
                    <StatusDot color={m.color} size={7} pulse={m.pulse} />
                    <span className="min-w-0 flex-1 truncate text-[12.5px] font-medium text-foreground">{sessionTitle(s)}</span>
                    <span className="shrink-0 text-[11.5px] text-muted-foreground">Claude Code</span>
                  </button>
                );
              })}
            </>
          )}
          {gwJobs.length > 0 && (
            <>
              <div className={`${sectionLabel} ${gwSessions.length > 0 ? "border-t border-border" : ""}`}>
                Scheduled jobs · {gwJobs.length}
              </div>
              {gwJobs.map((j) => (
                <button key={j.id} type="button" onClick={() => nav.navigate({ kind: "jobDetail", id: j.id })} className={runRow}>
                  <Clock aria-hidden size={12} strokeWidth={2} className="shrink-0 text-muted-foreground" />
                  <span className="min-w-0 flex-1 truncate text-[12.5px] font-medium text-foreground">{j.name}</span>
                  <span className="shrink-0 rounded-full bg-secondary px-1.5 py-px font-mono text-[10.5px] text-secondary-foreground">
                    {j.cron}
                  </span>
                  <span className="shrink-0 text-[11.5px] text-muted-foreground">{j.next}</span>
                </button>
              ))}
            </>
          )}
          {gwApps.length > 0 && (
            <>
              <div className={`${sectionLabel} ${gwSessions.length > 0 || gwJobs.length > 0 ? "border-t border-border" : ""}`}>
                Apps · {gwApps.length}
              </div>
              <div className="flex flex-wrap gap-1.5 px-[18px] pb-3.5 pt-1.5">
                {gwApps.map((a) => (
                  <button
                    key={a.id}
                    type="button"
                    onClick={() => nav.navigate({ kind: "appDetail", id: a.id })}
                    className="flex h-7 cursor-pointer items-center gap-[7px] rounded-full border border-border bg-transparent pl-1.5 pr-[11px] font-sans text-xs font-medium text-foreground hover:bg-accent"
                  >
                    <Chip initial={a.initial} color={a.color} size={18} mono className="rounded-full" />
                    {a.name}
                  </button>
                ))}
              </div>
            </>
          )}
        </Card>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Security</CardTitle>
          </CardHeader>
          <div className="flex items-center gap-2.5 border-b border-border px-[18px] py-[11px]">
            <span className="w-[120px] shrink-0 text-[12.5px] font-medium">Host fingerprint</span>
            {fp ? (
              <>
                <span className="min-w-0 flex-1 truncate font-mono text-[11px] text-muted-foreground">{fp}</span>
                <span className="flex shrink-0 items-center gap-1 text-[11px]" style={{ color: "#22C55E" }}>
                  <Check aria-hidden size={11} strokeWidth={2.5} />
                  Verified Jun 12
                </span>
                <button type="button" onClick={() => void navigator.clipboard.writeText(fp)} className={smallBtn}>
                  Copy
                </button>
              </>
            ) : (
              <span className="flex-1 text-xs text-muted-foreground">Local machine — no host key needed.</span>
            )}
          </div>
          <div className="flex flex-col gap-2 px-[18px] py-3">
            <div className="flex items-center gap-3">
              <span className="flex-1 text-[12.5px] font-medium">Filesystem access</span>
              <Segmented
                options={GW_FS_MODES.map((m) => ({ id: m.id, label: m.label }))}
                value={gw.fsMode}
                onChange={(m) => setGatewayFsMode(id, m)}
              />
            </div>
            <div className="text-right text-[11.5px] text-muted-foreground">{fsDesc}</div>
            {gw.fsMode === "projects" && (
              <div className="flex flex-wrap gap-1.5">
                {w.paths.map((p) => (
                  <span
                    key={p}
                    className="flex h-[26px] items-center rounded-full border border-border px-2.5 font-mono text-[11px] text-foreground"
                  >
                    {p}
                  </span>
                ))}
                <button
                  type="button"
                  className="flex h-[26px] cursor-pointer items-center gap-[5px] rounded-full border border-dashed border-border bg-transparent px-2.5 font-sans text-[11.5px] text-muted-foreground hover:bg-accent hover:text-accent-foreground"
                >
                  <Plus aria-hidden size={10} strokeWidth={2} />
                  Add folder
                </button>
              </div>
            )}
          </div>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Log</CardTitle>
            <CardHint>Daemon events, most recent last</CardHint>
            <span className="flex-1" />
            <button type="button" className={smallBtn}>
              Copy
            </button>
          </CardHeader>
          <div className="overflow-x-auto bg-code px-[18px] py-3 font-mono text-[11.5px] leading-[1.75] text-code-foreground">
            {w.log.map((l, i) => (
              <div key={`${i}-${l.t}`} className="whitespace-pre-wrap" style={{ color: l.c }}>
                {l.t}
              </div>
            ))}
          </div>
        </Card>
      </div>
    </div>
  );
}
