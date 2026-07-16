import type { AgentRun, Message } from "../bindings";
import { formatTurnDuration, groupRows, messageToRow, type ActivityItem } from "./transcript";

const EXCERPT_LIMIT = 280;

export type DispatchSlot = {
  dispatchIndex: number;
  attempts: AgentRun[];
  current: AgentRun;
  attemptNumber: number;
};

export type AgentRunPreviewModel = {
  task: string;
  activities: ActivityItem[];
  excerpt: string | null;
};

export type AgentRunStatusPresentation = {
  label: string;
  tone: string;
};

const statusPresentation: Record<AgentRun["status"], AgentRunStatusPresentation> = {
  queued: { label: "Queued", tone: "text-muted-foreground" },
  running: { label: "Running", tone: "text-primary" },
  completed: { label: "Completed", tone: "text-emerald-600 dark:text-emerald-400" },
  failed: { label: "Failed", tone: "text-destructive" },
  cancelled: { label: "Cancelled", tone: "text-muted-foreground" },
  interrupted: { label: "Interrupted", tone: "text-amber-700 dark:text-amber-400" },
};

export function agentRunStatusPresentation(status: AgentRun["status"]): AgentRunStatusPresentation {
  return statusPresentation[status];
}

export function dispatchSlotKey(ownerRunId: string, sourceToolCallId: string, dispatchIndex: number): string {
  return `${ownerRunId}\u0000${sourceToolCallId}\u0000${dispatchIndex}`;
}

function excerpt(text: string): string {
  const lineSafe = text.trim().replace(/\s+/g, " ");
  return lineSafe.length <= EXCERPT_LIMIT ? lineSafe : `${lineSafe.slice(0, EXCERPT_LIMIT - 1)}…`;
}

function attemptDepth(run: AgentRun, byId: Map<string, AgentRun>, seen = new Set<string>()): number {
  if (!run.retryOf || seen.has(run.runId)) return 0;
  const previous = byId.get(run.retryOf);
  if (!previous) return 0;
  const nextSeen = new Set(seen);
  nextSeen.add(run.runId);
  return attemptDepth(previous, byId, nextSeen) + 1;
}

function compareAttempts(a: AgentRun, b: AgentRun, byId: Map<string, AgentRun>): number {
  return (
    attemptDepth(a, byId) - attemptDepth(b, byId) ||
    (a.startedAt ?? 0) - (b.startedAt ?? 0) ||
    a.runId.localeCompare(b.runId)
  );
}

function selectTip(attempts: AgentRun[], byId: Map<string, AgentRun>): AgentRun {
  const referenced = new Set(attempts.flatMap((attempt) => (attempt.retryOf ? [attempt.retryOf] : [])));
  const candidates = attempts.filter((attempt) => !referenced.has(attempt.runId));
  return (candidates.length ? candidates : attempts)
    .slice()
    .sort((a, b) => compareAttempts(b, a, byId))[0]!;
}

export function linkedDispatchSlots(ownerRunId: string | null, toolCallId: string | null, runs: AgentRun[]): DispatchSlot[] {
  if (!ownerRunId || !toolCallId) return [];
  const byId = new Map(runs.map((run) => [run.runId, run]));
  const grouped = new Map<number, AgentRun[]>();
  for (const run of runs) {
    if (run.parentRunId !== ownerRunId || run.sourceToolCallId !== toolCallId || run.dispatchIndex === null) continue;
    const attempts = grouped.get(run.dispatchIndex) ?? [];
    attempts.push(run);
    grouped.set(run.dispatchIndex, attempts);
  }
  return [...grouped]
    .map(([dispatchIndex, attempts]) => {
      const ordered = attempts.slice().sort((a, b) => compareAttempts(a, b, byId));
      const current = selectTip(ordered, byId);
      return { dispatchIndex, attempts: ordered, current, attemptNumber: attemptDepth(current, byId) + 1 };
    })
    .sort((a, b) => a.dispatchIndex - b.dispatchIndex);
}

function liveActivities(messages: Message[]): ActivityItem[] {
  const rows = messages.map((message) =>
    messageToRow(
      message.seq,
      message.role,
      message.blockType,
      message.payload,
      message.toolCallId,
      message.status,
      message.toolKind,
      message.createdAt,
      message.sessionPk,
    ),
  );
  return groupRows(rows).flatMap((group) => (group.type === "activity" ? group.items : []));
}

function latestLiveExcerpt(messages: Message[]): string | null {
  for (let index = messages.length - 1; index >= 0; index -= 1) {
    const message = messages[index]!;
    const payload = message.payload as Record<string, unknown>;
    const text = message.blockType === "status" ? payload.summary : payload.text;
    if (typeof text === "string" && text.trim()) return excerpt(text);
  }
  return null;
}

export function projectAgentRunPreview(run: AgentRun, messages: Message[]): AgentRunPreviewModel {
  const terminal = run.status === "completed" || run.status === "failed" || run.status === "cancelled" || run.status === "interrupted";
  const terminalText = run.result ?? run.error;
  return {
    task: excerpt(run.task),
    activities: liveActivities(messages).slice(-3),
    excerpt: terminal ? (terminalText?.trim() ? excerpt(terminalText) : run.status === "completed" ? "Completed with no report." : null) : latestLiveExcerpt(messages),
  };
}

export function kindLabel(run: AgentRun): "Subagent" | "Main agent" {
  return run.agentKind === "subagent" ? "Subagent" : "Main agent";
}

export function formatAgentRunDuration(run: AgentRun, now = Date.now()): string {
  if (run.startedAt === null) return "";
  return formatTurnDuration(Math.max(0, (run.finishedAt ?? now) - run.startedAt));
}
