import { Bot, CircleAlert, Clock3, Wrench } from "lucide-react";
import { Button } from "@ryuzi/ui";
import type { AgentRun } from "@/bindings";
import { formatAgentRunDuration, kindLabel, type AgentRunPreviewModel } from "@/lib/agent-runs";
import { AgentRunPreview } from "./AgentRunPreview";

export type AgentRunCardProps = {
  run: AgentRun;
  attemptNumber: number;
  preview: AgentRunPreviewModel;
  selected: boolean;
  onSelect: () => void;
};

const statusTone: Record<AgentRun["status"], string> = {
  queued: "text-muted-foreground",
  running: "text-primary",
  completed: "text-emerald-600 dark:text-emerald-400",
  failed: "text-destructive",
  cancelled: "text-muted-foreground",
  interrupted: "text-amber-700 dark:text-amber-400",
};

const statusLabel: Record<AgentRun["status"], string> = {
  queued: "Queued",
  running: "Running",
  completed: "Completed",
  failed: "Failed",
  cancelled: "Cancelled",
  interrupted: "Interrupted",
};

function terminalFallback(run: AgentRun): string | null {
  if (run.status === "failed") return "Failed without an error report.";
  if (run.status === "cancelled") return "Cancelled before completion.";
  if (run.status === "interrupted") return "Interrupted before completion.";
  return null;
}

/** Compact card for the current attempt in one durable dispatch slot. */
export function AgentRunCard({ run, attemptNumber, preview, selected, onSelect }: AgentRunCardProps) {
  const duration = formatAgentRunDuration(run);
  const detail = run.status === "running" ? null : (preview.excerpt ?? terminalFallback(run));
  const metadata = [
    `${run.toolCount} ${run.toolCount === 1 ? "tool" : "tools"}`,
    duration,
    run.resolvedModel,
    run.resolvedEffort,
  ].filter((value): value is string => Boolean(value));
  const status = statusLabel[run.status];

  return (
    <Button
      variant="ghost"
      aria-label={`Open ${run.executingAgentNameSnapshot} agent run`}
      aria-pressed={selected}
      onClick={onSelect}
      onKeyDown={(event) => {
        if ((event.key === "Enter" || event.key === " ") && !event.repeat) {
          event.preventDefault();
          onSelect();
        }
      }}
      className={`h-auto w-full items-start justify-start rounded-md border px-3 py-2.5 text-left whitespace-normal hover:bg-accent/70 focus-visible:ring-2 focus-visible:ring-ring/60 ${
        selected ? "border-primary/50 bg-primary/5" : "border-border bg-background/40"
      }`}
    >
      <Bot aria-hidden size={15} strokeWidth={2} className="mt-0.5 shrink-0 text-muted-foreground" />
      <span className="min-w-0 flex-1">
        <span className="flex min-w-0 flex-wrap items-center gap-x-2 gap-y-1">
          <span className="truncate font-medium text-foreground">{run.executingAgentNameSnapshot}</span>
          <span className="rounded border border-border px-1.5 py-px text-[10.5px] text-muted-foreground">{kindLabel(run)}</span>
          {attemptNumber > 1 && <span className="rounded border border-border px-1.5 py-px text-[10.5px] text-muted-foreground">Retry {attemptNumber}</span>}
        </span>
        <span aria-live="polite" className={`mt-1 block text-[11px] font-medium ${statusTone[run.status]}`}>
          {status}
        </span>
        <span className="mt-1 block line-clamp-2 text-[12px] leading-relaxed text-foreground">{preview.task}</span>
        <span className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-[10.5px] text-muted-foreground">
          {metadata.map((entry, index) => (
            <span key={`${entry}-${index}`} className="inline-flex items-center gap-1">
              {index === 0 ? <Wrench aria-hidden size={10} strokeWidth={2} /> : index === 1 ? <Clock3 aria-hidden size={10} strokeWidth={2} /> : null}
              {entry}
            </span>
          ))}
        </span>
        {detail && (
          <span className={`mt-2 block line-clamp-3 text-[11.5px] leading-relaxed ${run.status === "failed" ? "text-destructive" : "text-muted-foreground"}`}>
            {run.status === "failed" && <CircleAlert aria-hidden size={11} strokeWidth={2} className="mr-1 inline-block align-[-1px]" />}
            {detail}
          </span>
        )}
        {run.status === "running" && <AgentRunPreview preview={preview} />}
      </span>
    </Button>
  );
}
