import type { ReactNode } from "react";
import { AlertTriangle, Bot, RefreshCw } from "lucide-react";
import { Button } from "@ryuzi/ui";
import type { ActivityItem } from "@/lib/transcript";
import { linkedDispatchSlots, projectAgentRunPreview } from "@/lib/agent-runs";
import { useDelegation, delegationRunKey, delegationSessionKey } from "@/store-delegation";
import { useNav } from "@/store-nav";
import { AgentRunCard } from "./AgentRunCard";

export type AgentDispatchGroupProps = {
  runnerId: string;
  sessionPk: string;
  ownerRunId: string | null;
  item: Extract<ActivityItem, { type: "tool" }>;
  fallback: ReactNode;
};

const idleRosterState = { status: "idle" as const, error: null };

function isTerminalToolStatus(status: string | null): boolean {
  return status === "completed" || status === "failed" || status === "cancelled" || status === "interrupted";
}

function knownUnavailableIndices(
  ownerRunId: string | null,
  toolCallId: string | null,
  seenRuns: Record<string, string[]> | undefined,
  resolvedIndices: Set<number>,
): number[] {
  if (!ownerRunId || !toolCallId || !seenRuns) return [];
  const prefix = `${ownerRunId}\u0000${toolCallId}\u0000`;
  return Object.entries(seenRuns)
    .flatMap(([key, runIds]) => {
      if (!key.startsWith(prefix) || runIds.length === 0) return [];
      const index = Number(key.slice(prefix.length));
      return Number.isInteger(index) && index >= 0 && !resolvedIndices.has(index) ? [index] : [];
    })
    .sort((a, b) => a - b);
}

function LoadingCard() {
  return (
    <div
      role="status"
      aria-label="Loading agent run"
      className="flex h-[118px] w-full max-w-[640px] animate-pulse items-center gap-2 rounded-md border border-border bg-muted/20 px-3"
    >
      <Bot aria-hidden size={15} className="text-muted-foreground" />
      <span className="h-3 w-40 rounded bg-muted" />
    </div>
  );
}

function UnavailableCard() {
  return (
    <div role="status" className="flex min-h-[96px] w-full max-w-[640px] items-center gap-2 rounded-md border border-border bg-muted/20 px-3 text-[12px] text-muted-foreground">
      <AlertTriangle aria-hidden size={14} strokeWidth={2} />
      Agent run unavailable
    </div>
  );
}

function RetryLoad({ load, label }: { load: () => void; label: string }) {
  return (
    <Button variant="ghost" size="xs" onClick={load} className="max-w-fit text-muted-foreground">
      <RefreshCw aria-hidden size={12} strokeWidth={2} /> {label}
    </Button>
  );
}

/** Resolves the durable agent-run slots for one dispatch tool row. */
export function AgentDispatchGroup({ runnerId, sessionPk, ownerRunId, item, fallback }: AgentDispatchGroupProps) {
  const sessionKey = delegationSessionKey(runnerId, sessionPk);
  const runs = useDelegation((state) => state.bySession[sessionKey] ?? []);
  const rosterState = useDelegation((state) => state.rosterStateBySession[sessionKey] ?? idleRosterState);
  const seenRuns = useDelegation((state) => state.seenRunsByDispatch[sessionKey]);
  const selectedRunId = useDelegation((state) => state.selectedBySession[sessionKey] ?? null);
  const transcripts = useDelegation((state) => state.transcriptByRun);
  const load = useDelegation((state) => state.load);
  const select = useDelegation((state) => state.select);
  const setRightOpen = useNav((state) => state.setRightOpen);
  const setRightTab = useNav((state) => state.setRightTab);
  const slots = linkedDispatchSlots(ownerRunId, item.toolCallId, runs);
  const unavailable =
    rosterState.status === "ready" ? knownUnavailableIndices(ownerRunId, item.toolCallId, seenRuns, new Set(slots.map((slot) => slot.dispatchIndex))) : [];
  const rows = [
    ...slots.map((slot) => ({ dispatchIndex: slot.dispatchIndex, slot })),
    ...unavailable.map((dispatchIndex) => ({ dispatchIndex, slot: null })),
  ].sort((a, b) => a.dispatchIndex - b.dispatchIndex);
  const reload = () => void load(runnerId, sessionPk);

  if (slots.length === 0 && unavailable.length === 0) {
    if (rosterState.status === "idle" || rosterState.status === "loading") return <LoadingCard />;
    if (rosterState.status === "error") {
      return (
        <div className="flex max-w-[640px] flex-col gap-2">
          <div role="status" className="flex min-h-[96px] items-center gap-2 rounded-md border border-border bg-muted/20 px-3 text-[12px] text-muted-foreground">
            <AlertTriangle aria-hidden size={14} strokeWidth={2} /> Agent runs could not be loaded.
          </div>
          <RetryLoad load={reload} label="Retry loading agent runs" />
        </div>
      );
    }
    return isTerminalToolStatus(item.status) ? fallback : <LoadingCard />;
  }

  return (
    <div className="flex w-full max-w-[640px] flex-col gap-1.5">
      {rosterState.status === "error" && (
        <div role="status" className="flex flex-wrap items-center gap-2 rounded-md border border-amber-500/30 bg-amber-500/10 px-2.5 py-1.5 text-[11px] text-amber-800 dark:text-amber-200">
          <AlertTriangle aria-hidden size={12} strokeWidth={2} />
          Could not refresh agent runs.
          <RetryLoad load={reload} label="Retry loading agent runs" />
        </div>
      )}
      {rows.map(({ dispatchIndex, slot }) => {
        if (slot === null) return <UnavailableCard key={`unavailable-${dispatchIndex}`} />;
        const current = slot.current;
        const preview = projectAgentRunPreview(current, transcripts[delegationRunKey(runnerId, sessionPk, current.runId)] ?? []);
        return (
          <AgentRunCard
            key={current.runId}
            run={current}
            attemptNumber={slot.attemptNumber}
            preview={preview}
            selected={selectedRunId === current.runId}
            onSelect={() => {
              setRightOpen(true);
              setRightTab("agents");
              select(runnerId, sessionPk, current.runId);
            }}
          />
        );
      })}
    </div>
  );
}
