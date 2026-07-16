import { Bot, CircleAlert, Clock3 } from "lucide-react";
import type { AgentRun } from "@/bindings";
import { useDelegation, delegationSessionKey } from "@/store-delegation";
import { Button } from "@ryuzi/ui";

const activeStatuses = new Set(["queued", "running"]);

function kindLabel(run: AgentRun): string {
  return run.agentKind === "subagent" ? "Subagent" : "Main agent";
}

function duration(run: AgentRun): string {
  if (run.startedAt === null) return run.status === "queued" ? "Queued" : "—";
  const end = run.finishedAt ?? Date.now();
  const seconds = Math.max(0, Math.floor((end - run.startedAt) / 1_000));
  return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

function RunCard({ run, onSelect }: { run: AgentRun; onSelect: () => void }) {
  return (
    <Button variant="ghost" onClick={onSelect} className="h-auto w-full justify-start rounded-md px-3 py-2 text-left hover:bg-accent">
      <Bot aria-hidden size={14} className="mt-0.5 shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1">
        <span className="flex items-center gap-2">
          <span className="truncate font-medium text-foreground">{run.executingAgentNameSnapshot}</span>
          <span className="shrink-0 text-[11px] text-muted-foreground">{kindLabel(run)}</span>
        </span>
        <span className="mt-0.5 block truncate text-[12px] text-muted-foreground">{run.task}</span>
        <span className="mt-1 flex items-center gap-2 text-[11px] text-muted-foreground">
          <span className="capitalize">{run.status}</span>
          <span>
            {run.toolCount} {run.toolCount === 1 ? "tool" : "tools"}
          </span>
          <span className="inline-flex items-center gap-1">
            <Clock3 aria-hidden size={10} />
            {duration(run)}
          </span>
        </span>
        {run.error && (
          <span className="mt-1 flex items-center gap-1 text-[11px] text-destructive">
            <CircleAlert aria-hidden size={11} />
            {run.error}
          </span>
        )}
      </span>
    </Button>
  );
}

export function AgentRunRoster({ runnerId, sessionPk }: { runnerId: string; sessionPk: string }) {
  const key = delegationSessionKey(runnerId, sessionPk);
  const runs = useDelegation((state) => state.bySession[key] ?? []);
  const select = useDelegation((state) => state.select);
  const active = runs.filter((run) => activeStatuses.has(run.status));
  const done = runs.filter((run) => !activeStatuses.has(run.status));

  if (runs.length === 0) return <div className="p-4 text-[12.5px] text-muted-foreground">No child agents dispatched in this session.</div>;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto p-2">
      <section>
        <h3 className="px-2 py-1.5 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">Active ({active.length})</h3>
        <div className="flex flex-col gap-0.5">
          {active.map((run) => (
            <RunCard key={run.runId} run={run} onSelect={() => select(runnerId, sessionPk, run.runId)} />
          ))}
        </div>
      </section>
      <section className="mt-3">
        <h3 className="px-2 py-1.5 text-[11px] font-semibold uppercase tracking-wide text-muted-foreground">Done ({done.length})</h3>
        <div className="flex flex-col gap-0.5">
          {done.map((run) => (
            <RunCard key={run.runId} run={run} onSelect={() => select(runnerId, sessionPk, run.runId)} />
          ))}
        </div>
      </section>
    </div>
  );
}
