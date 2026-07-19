import { useState } from "react";
import { ArrowLeft, Bot, Copy, RotateCw, Square, Waypoints } from "lucide-react";
import type { AgentRun } from "@/bindings";
import { agentRunStatusPresentation, formatAgentRunDuration, kindLabel } from "@/lib/agent-runs";
import { useNow } from "@/hooks/useNow";
import { messageToRow } from "@/lib/transcript";
import { useDelegation, delegationRunKey } from "@/store-delegation";
import { Transcript } from "@/components/transcript/Transcript";
import { Markdown } from "@/components/transcript/Markdown";
import { ContextRing } from "./ContextRing";
import { ContextCostMenu } from "./ContextCostMenu";
import { useStore } from "@/store";
import { Button } from "@ryuzi/ui";

const activeStatuses = new Set(["queued", "running"]);
const retryableStatuses = new Set(["failed", "cancelled", "interrupted"]);

/** Isolated so the 1 Hz tick re-renders only this duration text, not AgentRunDetail's rows/Transcript. */
function RunHeaderDuration({ run, active }: { run: AgentRun; active: boolean }) {
  const now = useNow(active);
  const duration = formatAgentRunDuration(run, now);
  if (!duration) return null;
  return <span>{duration}</span>;
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
  const transcriptKey = delegationRunKey(runnerId, sessionPk, run.runId);
  const transcript = useDelegation((state) => state.transcriptByRun[transcriptKey] ?? []);
  const transcriptState = useDelegation((state) => state.transcriptStateByRun[transcriptKey]);
  const select = useDelegation((state) => state.select);
  const loadTranscript = useDelegation((state) => state.loadTranscript);
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
    ),
  );
  const active = activeStatuses.has(run.status);
  const status = agentRunStatusPresentation(run.status);
  const live = useStore((s) => s.runContextUsage[transcriptKey]);
  const liveCost = useStore((s) => s.runCost[transcriptKey]);
  const usage =
    live ??
    (run.contextPercentLeft != null
      ? {
          activeTokens: run.contextActiveTokens ?? 0,
          usableWindow: run.contextUsableWindow ?? 0,
          percentLeft: run.contextPercentLeft,
          contextWindow: run.contextWindow ?? 0,
          cacheReadTokens: run.cacheReadTokens ?? 0,
          cacheCreationTokens: run.cacheCreationTokens ?? 0,
          outputTokens: run.outputTokens ?? 0,
        }
      : undefined);
  const cost = liveCost ?? run.cost ?? undefined;
  const [costOpen, setCostOpen] = useState(false);

  return (
    <div className="min-h-0 flex flex-1 flex-col">
      <header className="flex shrink-0 flex-wrap items-center gap-x-2 gap-y-1.5 border-b border-border px-3 py-2">
        <Button
          variant="ghost"
          size="icon-sm"
          aria-label="Back to agent runs"
          title="Back"
          onClick={() => select(runnerId, sessionPk, null)}
          className="-ml-1"
        >
          <ArrowLeft aria-hidden size={15} />
        </Button>
        <div className="flex min-w-0 flex-1 items-center gap-2">
          <div
            role="img"
            aria-label={`Agent avatar for ${run.executingAgentNameSnapshot}`}
            className="flex size-6 shrink-0 items-center justify-center rounded-full bg-muted text-muted-foreground"
          >
            <Bot aria-hidden size={13} />
          </div>
          <span className="min-w-0 truncate font-medium">{run.executingAgentNameSnapshot}</span>
        </div>
        <div className="order-3 flex w-full flex-wrap items-center gap-x-2 gap-y-1 text-[10.5px] text-muted-foreground">
          <span>{kindLabel(run)}</span>
          <span className={`font-medium ${status.tone}`}>{status.label}</span>
          <span>
            {run.toolCount} {run.toolCount === 1 ? "tool" : "tools"}
          </span>
          <RunHeaderDuration run={run} active={active} />
          {run.resolvedModel && <span>{run.resolvedModel}</span>}
          {run.resolvedEffort && <span>{run.resolvedEffort}</span>}
          {usage && (
            <div className="relative inline-flex items-center">
              <Button
                variant="ghost"
                size="xs"
                aria-expanded={costOpen}
                aria-label="Context and cost"
                title={`Sub-agent context: ~${usage.activeTokens.toLocaleString()} of ${usage.usableWindow.toLocaleString()} tokens used`}
                onClick={() => setCostOpen((v) => !v)}
                className="h-auto gap-1.5 p-0 hover:bg-transparent dark:hover:bg-transparent"
              >
                <ContextRing percentLeft={usage.percentLeft} />
              </Button>
              {costOpen && (
                <ContextCostMenu
                  onClose={() => setCostOpen(false)}
                  className="top-6 right-0 w-[260px]"
                  usage={{
                    activeTokens: usage.activeTokens,
                    usableWindow: usage.usableWindow,
                    contextWindow: usage.contextWindow,
                    cacheReadTokens: usage.cacheReadTokens,
                  }}
                  cost={cost}
                />
              )}
            </div>
          )}
        </div>
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
          {run.error && <p className="mb-0 mt-2 text-[12px] text-destructive">{run.error}</p>}
          {run.result && (
            <div className="mt-3 rounded-md border border-border bg-muted/30 p-2.5">
              <div className="mb-1 flex items-center justify-between gap-2 text-[11px] font-medium text-muted-foreground">
                Final result
                <Button
                  variant="ghost"
                  size="xs"
                  aria-label="Copy result"
                  onClick={() => void navigator.clipboard.writeText(run.result ?? "")}
                >
                  <Copy aria-hidden size={12} /> Copy
                </Button>
              </div>
              <div className="text-[12.5px]">
                <Markdown text={run.result} />
              </div>
            </div>
          )}
          <Button variant="ghost" size="sm" onClick={onRelatedChanges} className="mt-2 -ml-2 text-muted-foreground">
            <Waypoints aria-hidden size={13} /> Related changes
          </Button>
        </div>
        {transcriptState?.status === "error" && (
          <div
            role="alert"
            className="flex flex-wrap items-center gap-2 border-b border-destructive/30 bg-destructive/5 px-4 py-2 text-[12px] text-destructive"
          >
            <span>{transcriptState.error ?? "Could not load transcript."}</span>
            <Button
              variant="ghost"
              size="xs"
              onClick={() => void loadTranscript(runnerId, sessionPk, run.runId)}
              className="text-destructive"
            >
              Retry transcript
            </Button>
          </div>
        )}
        <div className="min-h-[240px]">
          <Transcript
            runnerId={runnerId}
            sessionPk={sessionPk}
            rows={rows}
            agentColor="#6b7280"
            running={active}
            ownerRunId={run.runId}
            approvalRunId={run.runId}
          />
        </div>
      </div>
    </div>
  );
}
