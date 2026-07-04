import { ChevronRight, Clock, Folder, Plus } from "lucide-react";
import { useEffect } from "react";
import { runtimeById, useRuntimes } from "@/store-runtimes";
import { formatNextRun, formatStarted, useScheduler } from "@/store-scheduler";
import { useGateways } from "@/store-gateways";
import { useNav } from "@/store-nav";
import { Card } from "@/components/common/Card";
import { Switch } from "@/components/common/Switch";
import { Pill, StatusDot } from "@/components/common/bits";

const RUN_COLORS: Record<string, string> = { success: "#22C55E", failed: "#EF4444", running: "#3B82F6" };

export function SchedulerView() {
  const { jobs, loaded, hydrate, toggle } = useScheduler();
  const { gateways, loaded: gwLoaded, hydrate: hydrateGw } = useGateways();
  const runtimes = useRuntimes((s) => s.runtimes);
  const nav = useNav();

  useEffect(() => {
    void hydrate();
    if (!gwLoaded) void hydrateGw();
  }, [hydrate, gwLoaded, hydrateGw]);

  // Group real jobs under their gateway; unknown gateways group under local.
  const groups = gateways.map((w) => ({ gateway: w, jobs: jobs.filter((j) => j.gateway === w.id) })).filter((g) => g.jobs.length > 0);
  const orphaned = jobs.filter((j) => !gateways.some((w) => w.id === j.gateway));
  if (orphaned.length > 0 && gateways.length > 0) {
    const local = groups.find((g) => g.gateway.id === "local");
    if (local) local.jobs.push(...orphaned);
    else groups.push({ gateway: gateways[0], jobs: orphaned });
  }

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <div className="mb-5 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Scheduler</h2>
            <p className="m-0 text-[13px] text-muted-foreground">
              Recurring agent runs. Schedules fire while Cockpit is running and start a fresh session with the job's prompt.
            </p>
          </div>
          <button
            type="button"
            onClick={() => nav.navigate({ kind: "jobNew" })}
            className="flex h-8 shrink-0 cursor-pointer items-center gap-[7px] rounded-md border-none bg-primary px-3 font-sans text-[12.5px] font-medium text-primary-foreground hover:opacity-85"
          >
            <Plus aria-hidden size={14} strokeWidth={2} />
            New job
          </button>
        </div>

        {loaded && jobs.length === 0 && (
          <Card className="p-6 text-center text-[13px] text-muted-foreground">
            No scheduled jobs yet. Create one — the prompt runs on a fresh session at every scheduled time.
          </Card>
        )}

        <div className="flex flex-col gap-5">
          {groups.map(({ gateway: w, jobs: groupJobs }) => {
            const offline = w.status === "offline";
            const statusColor = offline ? "#9CA3AF" : "#22C55E";
            return (
              <div key={w.id} className="flex flex-col gap-2">
                <div className="flex items-center gap-2 px-0.5">
                  <span className="flex h-6 w-6 shrink-0 items-center justify-center rounded-sm bg-muted font-mono text-[8px] font-semibold text-muted-foreground">
                    {w.badge}
                  </span>
                  <button
                    type="button"
                    onClick={() => nav.navigate({ kind: "gatewayDetail", id: w.id })}
                    className="cursor-pointer border-none bg-transparent p-0 font-sans text-[12.5px] font-semibold text-foreground hover:underline"
                  >
                    {w.name}
                  </button>
                  <span className="flex items-center gap-[5px] text-[11px]" style={{ color: statusColor }}>
                    <StatusDot color={statusColor} size={6} pulse={false} />
                    {offline ? "Offline" : "Connected"}
                  </span>
                  <span className="text-[11px] text-muted-foreground">· Cockpit {w.daemonVersion} runs these</span>
                </div>
                {groupJobs.map((j) => {
                  const open = () => nav.navigate({ kind: "jobDetail", id: j.id });
                  const agent = runtimeById(runtimes, j.agent);
                  return (
                    <Card key={j.id} className="flex items-center gap-3.5 px-[18px] py-[15px]">
                      <button
                        type="button"
                        onClick={open}
                        className="flex min-w-0 flex-1 cursor-pointer items-center gap-3.5 border-none bg-transparent p-0 text-left font-sans text-foreground"
                      >
                        <span className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-muted text-muted-foreground">
                          <Clock aria-hidden size={17} strokeWidth={2} />
                        </span>
                        <span className="min-w-0 flex-1">
                          <span className="flex items-center gap-2">
                            <span className="overflow-hidden text-ellipsis whitespace-nowrap text-sm font-semibold">{j.name}</span>
                            <Pill variant="mono" className="shrink-0">
                              {j.cron}
                            </Pill>
                          </span>
                          <span className="mt-1 flex items-center gap-1.5 text-xs text-muted-foreground">
                            <Folder aria-hidden size={12} strokeWidth={2} className="shrink-0" />
                            <span>{j.projectName}</span>
                            <span className="opacity-40">·</span>
                            <span>{agent?.name ?? j.agent}</span>
                          </span>
                          {j.history.length > 0 && (
                            <span className="mt-[7px] flex items-center gap-[5px]">
                              {j.history.slice(0, 5).map((r) => (
                                <span
                                  key={r.id}
                                  className="h-1.5 w-1.5 rounded-full"
                                  style={{ background: RUN_COLORS[r.status] ?? "#9CA3AF" }}
                                />
                              ))}
                              <span className="ml-1 text-[11px] text-muted-foreground">
                                Last run {formatStarted(j.history[0].startedAtMs)}
                              </span>
                            </span>
                          )}
                        </span>
                      </button>
                      <div className="shrink-0 text-right">
                        <div className="text-[11px] text-muted-foreground">Next run</div>
                        <div className="text-[12.5px] font-semibold">{j.enabled ? formatNextRun(j.nextRunMs) : "Paused"}</div>
                      </div>
                      <Switch on={j.enabled} onToggle={() => void toggle(j.id, !j.enabled)} label={`Enable ${j.name}`} />
                      <button
                        type="button"
                        title="Details"
                        onClick={open}
                        className="flex h-7 w-7 shrink-0 cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground"
                      >
                        <ChevronRight aria-hidden size={14} strokeWidth={2} />
                      </button>
                    </Card>
                  );
                })}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
}
