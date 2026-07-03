import { ChevronRight, Clock, Folder, Plus } from "lucide-react";
import { AGENTS, WORKSPACES, type JobRun } from "@/fixtures";
import { useFixtures } from "@/store-fixtures";
import { useNav } from "@/store-nav";
import { Card } from "@/components/common/Card";
import { Switch } from "@/components/common/Switch";
import { Pill } from "@/components/common/bits";

const RUN_COLORS: Record<JobRun["status"], string> = { success: "#22C55E", failed: "#EF4444", running: "#3B82F6" };

export function SchedulerView() {
  const { jobs, toggleJob } = useFixtures();
  const nav = useNav();

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <div className="mb-5 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Scheduler</h2>
            <p className="m-0 text-[13px] text-muted-foreground">
              Recurring agent runs. Jobs execute on their workspace gateway, even when Cockpit is closed.
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

        <div className="flex flex-col gap-2.5">
          {jobs.map((j) => {
            const open = () => nav.navigate({ kind: "jobDetail", id: j.id });
            const wsName = WORKSPACES.find((w) => w.id === j.workspace)?.name ?? j.workspace;
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
                      <span>{j.project}</span>
                      <span className="opacity-40">·</span>
                      <span>{AGENTS[j.agent].name}</span>
                      <span className="opacity-40">·</span>
                      <span>{wsName}</span>
                    </span>
                    {j.history.length > 0 && (
                      <span className="mt-[7px] flex items-center gap-[5px]">
                        {j.history.slice(0, 5).map((r) => (
                          <span key={r.id} className="h-1.5 w-1.5 rounded-full" style={{ background: RUN_COLORS[r.status] }} />
                        ))}
                        <span className="ml-1 text-[11px] text-muted-foreground">Last run {j.history[0].started}</span>
                      </span>
                    )}
                  </span>
                </button>
                <div className="shrink-0 text-right">
                  <div className="text-[11px] text-muted-foreground">Next run</div>
                  <div className="text-[12.5px] font-semibold">{j.next}</div>
                </div>
                <Switch on={j.on} onToggle={() => toggleJob(j.id)} label={`Enable ${j.name}`} />
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
      </div>
    </div>
  );
}
