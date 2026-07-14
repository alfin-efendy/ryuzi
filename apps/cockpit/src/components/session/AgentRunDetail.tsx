import { ArrowLeft, Copy, RotateCw, Square, Waypoints } from "lucide-react";
import type { AgentRun } from "@/bindings";
import { messageToRow } from "@/lib/transcript";
import { useDelegation, delegationRunKey } from "@/store-delegation";
import { Transcript } from "@/components/transcript/Transcript";
import { Button } from "@ryuzi/ui";

const activeStatuses = new Set(["queued", "running"]);
const retryableStatuses = new Set(["failed", "cancelled", "interrupted"]);

function duration(run: AgentRun): string {
  if (run.startedAt === null) return run.status === "queued" ? "Queued" : "—";
  const milliseconds = (run.finishedAt ?? Date.now()) - run.startedAt;
  const seconds = Math.max(0, Math.floor(milliseconds / 1_000));
  return seconds < 60 ? `${seconds}s` : `${Math.floor(seconds / 60)}m ${seconds % 60}s`;
}

export function AgentRunDetail({
  runnerId,
  sessionPk,
  run,
  onRelatedChanges,
}: {
  runnerId: string;
  sessionPk: string;
  run: AgentRun;
  onRelatedChanges: () => void;
}) {
  const transcript = useDelegation((state) => state.transcriptByRun[delegationRunKey(runnerId, sessionPk, run.runId)] ?? []);
  const select = useDelegation((state) => state.select);
  const stop = useDelegation((state) => state.stop);
  const retry = useDelegation((state) => state.retry);
  const rows = transcript.map((message) =>
    messageToRow(
      message.seq,
      message.role,
      message.blockType,
      message.payload,
      message.toolCallId,
      message.status,
      message.toolKind,
      message.createdAt,
      sessionPk,
      message.speaker,
    ),
  );
  const active = activeStatuses.has(run.status);

  return (
    <div className="min-h-0 flex flex-1 flex-col">
      <header className="flex shrink-0 items-center gap-2 border-b border-border px-3 py-2">
        <Button variant="ghost" size="sm" aria-label="Back to Agents" onClick={() => select(runnerId, sessionPk, null)} className="-ml-1">
          <ArrowLeft aria-hidden size={14} /> Back to Agents
        </Button>
        <span className="min-w-0 flex-1 truncate font-medium">{run.executingAgentNameSnapshot}</span>
        {active && (
          <Button variant="ghost" size="sm" onClick={() => void stop(runnerId, sessionPk, run.runId)} className="text-destructive">
            <Square aria-hidden size={13} /> Stop
          </Button>
        )}
        {retryableStatuses.has(run.status) && (
          <Button variant="ghost" size="sm" onClick={() => void retry(runnerId, sessionPk, run.runId)}>
            <RotateCw aria-hidden size={13} /> Retry
          </Button>
        )}
      </header>
      <div className="min-h-0 flex-1 overflow-y-auto">
        <div className="border-b border-border px-4 py-3">
          <h3 className="text-[13px] font-medium">{run.task}</h3>
          <div className="mt-1 flex flex-wrap gap-x-3 gap-y-1 text-[11px] text-muted-foreground">
            <span className="capitalize">{run.status}</span>
            <span>{run.agentKind === "subagent" ? "Subagent" : "Main agent"}</span>
            <span>{run.toolCount} {run.toolCount === 1 ? "tool" : "tools"}</span>
            <span>{duration(run)}</span>
            {run.resolvedModel && <span>{run.resolvedModel}{run.resolvedEffort ? ` · ${run.resolvedEffort}` : ""}</span>}
          </div>
          {run.error && <p className="mb-0 mt-2 text-[12px] text-destructive">{run.error}</p>}
          {run.result && (
            <div className="mt-3 rounded-md border border-border bg-muted/30 p-2.5">
              <div className="mb-1 flex items-center justify-between gap-2 text-[11px] font-medium text-muted-foreground">
                Final result
                <Button variant="ghost" size="xs" aria-label="Copy result" onClick={() => void navigator.clipboard.writeText(run.result ?? "")}>
                  <Copy aria-hidden size={12} /> Copy
                </Button>
              </div>
              <p className="mb-0 whitespace-pre-wrap text-[12.5px]">{run.result}</p>
            </div>
          )}
          <Button variant="ghost" size="sm" onClick={onRelatedChanges} className="mt-2 -ml-2 text-muted-foreground">
            <Waypoints aria-hidden size={13} /> Related changes
          </Button>
        </div>
        <div className="min-h-[240px]">
          <Transcript runnerId={runnerId} sessionPk={sessionPk} rows={rows} agentName={run.executingAgentNameSnapshot} agentColor="#6b7280" running={active} approvalRunId={run.runId} />
        </div>
      </div>
    </div>
  );
}
