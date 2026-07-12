// Pure transcript-shaping logic for the session chat: the store's Row shape
// and the render-time grouping of rows into visual blocks. No React, no I/O —
// tested directly (same pattern as lib/diff.ts).

export type Row = {
  /** DB seq; 0 for transient error events (they carry no wire seq). */
  seq: number;
  /** user | assistant | system */
  role: string;
  /** text | thought | tool_call | status | error (unknown values render as text) */
  blockType: string;
  /** text/thought chunk, status summary, or error message */
  text: string;
  toolCallId: string | null;
  /** pending | in_progress | completed | failed */
  toolStatus: string | null;
  toolKind: string | null;
  toolName: string | null;
  toolOutput: string | null;
  /** Wall-clock ms — from Message.createdAt on hydrate, Date.now() on live events. */
  createdAt: number | null;
  /** Saved attachment metadata (user rows only). */
  attachments: RowAttachment[];
  /** Target file of an edit/delete/move tool call, from its input. */
  toolPath: string | null;
  /** Raw tool input object from the payload (tool_call rows only). */
  toolInput: unknown;
  /** Wall-clock tool duration in ms (payload.duration_ms, native runner). */
  toolDurationMs: number | null;
  /** Structured process exit code (payload.exit_code, bash tool). */
  toolExitCode: number | null;
  /** One-line display summary (payload.summary — todo/task/memory tools). */
  toolSummary: string | null;
  /** Sub-agent label when this tool ran inside a dispatched sub-agent (payload.subagent). */
  toolSubagent: string | null;
  /** Worker/orchestrator agent name for group-chat rows (Message.speaker); null for ordinary assistant turns. */
  speaker: string | null;
  /** Blocking task id for an `orch_block` speaker row (payload.task_id); null for every other row. */
  taskId: string | null;
};

export type RowAttachment = { name: string; path: string; contentType: string | null; size: number };

export type ActivityItem =
  | {
      type: "tool";
      key: string;
      name: string;
      kind: string | null;
      status: string | null;
      output: string | null;
      path: string | null;
      input: unknown;
      durationMs: number | null;
      exitCode: number | null;
      summary: string | null;
      subagent: string | null;
    }
  | { type: "status"; key: string; text: string };

export type Group =
  | { type: "user"; key: string; text: string; attachments: RowAttachment[] }
  | { type: "agent"; key: string; markdown: string; turnEnd?: boolean }
  | { type: "thought"; key: string; markdown: string }
  | { type: "activity"; key: string; items: ActivityItem[] }
  | { type: "error"; key: string; text: string }
  | { type: "notice"; key: string; text: string }
  | { type: "speaker"; key: string; speaker: string; markdown: string; blockType: string; taskId: string | null };

export type EditCard = { path: string; kind: string };

export type TurnBlock = Group | { type: "summary"; key: string; groups: Group[]; durationMs: number | null; editCards: EditCard[] };

/** How many most-recent items stay visible in a live streaming run. */
export const STREAMING_TAIL = 3;

/** One piece of a partitioned activity cluster: a folded run of steps, or a
 *  standalone item that must stay visible. */
export type ActivityFragment = { kind: "fold"; items: ActivityItem[]; runLength: number } | { kind: "item"; item: ActivityItem };

/** True while the tool is still running — running items never fold. */
function isInProgress(item: ActivityItem): boolean {
  return item.type === "tool" && (item.status === "pending" || item.status === "in_progress");
}

/** Split a cluster into folded groups and standalone items.
 *
 *  `liveTail=true` (the cluster is the transcript tail while the agent runs):
 *  the last STREAMING_TAIL items stay visible; everything older folds.
 *  `liveTail=false` (the agent moved past this cluster): everything folds.
 *  In-progress items never fold in either branch and split the fold around
 *  them. Every fold carries `runLength` = the WHOLE cluster's size, so the
 *  "See N steps" label counts the run, not just its hidden part. */
export function partitionActivity(items: ActivityItem[], liveTail: boolean): ActivityFragment[] {
  const tailStart = liveTail ? Math.max(0, items.length - STREAMING_TAIL) : items.length;
  const fragments: ActivityFragment[] = [];
  let fold: ActivityItem[] = [];
  const flush = () => {
    if (fold.length === 0) return;
    fragments.push({ kind: "fold", items: fold, runLength: items.length });
    fold = [];
  };
  items.forEach((item, index) => {
    if ((liveTail && index >= tailStart) || isInProgress(item)) {
      flush();
      fragments.push({ kind: "item", item });
    } else {
      fold.push(item);
    }
  });
  flush();
  return fragments;
}

/** Distinct completed edit/delete/move/write tool targets, first-seen order,
 *  latest kind wins per path. */
export function editCardsForGroups(groups: Group[]): EditCard[] {
  const seen = new Map<string, string>();
  for (const g of groups) {
    if (g.type !== "activity") continue;
    for (const item of g.items) {
      if (item.type !== "tool" || item.status !== "completed" || !item.path) continue;
      if (item.kind === "edit" || item.kind === "delete" || item.kind === "move" || item.kind === "write") {
        seen.set(item.path, item.kind);
      }
    }
  }
  return Array.from(seen, ([path, kind]) => ({ path, kind }));
}

function outputPreview(v: unknown): string | null {
  if (v === undefined || v === null) return null;
  if (typeof v === "string") return v;
  return JSON.stringify(v, null, 2);
}

function rowAttachments(p: Record<string, unknown>): RowAttachment[] {
  if (!Array.isArray(p.attachments)) return [];
  return (p.attachments as Record<string, unknown>[]).flatMap((a) =>
    a && typeof a.path === "string"
      ? [
          {
            name: typeof a.name === "string" && a.name ? a.name : a.path,
            path: a.path,
            contentType: typeof a.contentType === "string" ? a.contentType : null,
            size: typeof a.size === "number" ? a.size : 0,
          },
        ]
      : [],
  );
}

function toolPathOf(p: Record<string, unknown>): string | null {
  const input = (p.input ?? {}) as Record<string, unknown>;
  if (typeof input.path === "string" && input.path) return input.path;
  if (typeof input.file_path === "string" && input.file_path) return input.file_path;
  return null;
}

// Projects a persisted/streamed message block onto the render Row shape.
// Unknown block types fall through as text (forward compatibility).
export function messageToRow(
  seq: number,
  role: string,
  blockType: string,
  payload: unknown,
  toolCallId: string | null,
  status: string | null,
  toolKind: string | null,
  createdAt: number | null,
  speaker: string | null = null,
): Row {
  const p = (payload ?? {}) as Record<string, unknown>;
  const text = blockType === "status" ? String(p.summary ?? "") : blockType === "error" ? String(p.message ?? "") : String(p.text ?? "");
  return {
    seq,
    role,
    blockType,
    text,
    toolCallId,
    toolStatus: status,
    toolKind,
    toolName: blockType === "tool_call" && typeof p.name === "string" && p.name ? p.name : null,
    toolOutput: blockType === "tool_call" ? outputPreview(p.output) : null,
    createdAt,
    attachments: rowAttachments(p),
    toolPath: blockType === "tool_call" ? toolPathOf(p) : null,
    toolInput: blockType === "tool_call" && p.input !== undefined ? p.input : null,
    toolDurationMs: blockType === "tool_call" && typeof p.duration_ms === "number" ? p.duration_ms : null,
    toolExitCode: blockType === "tool_call" && typeof p.exit_code === "number" ? p.exit_code : null,
    toolSummary: blockType === "tool_call" && typeof p.summary === "string" && p.summary ? p.summary : null,
    toolSubagent: blockType === "tool_call" && typeof p.subagent === "string" && p.subagent ? p.subagent : null,
    speaker,
    taskId: blockType === "orch_block" && typeof p.task_id === "string" && p.task_id ? p.task_id : null,
  };
}

// A tool-update re-emit re-uses the original row's seq: merge by identity.
export function mergeToolRow(prev: Row, payload: unknown, status: string | null, toolKind: string | null): Row {
  const p = (payload ?? {}) as Record<string, unknown>;
  return {
    ...prev,
    toolStatus: status ?? prev.toolStatus,
    toolKind: toolKind ?? prev.toolKind,
    toolName: typeof p.name === "string" && p.name ? p.name : prev.toolName,
    toolOutput: outputPreview(p.output) ?? prev.toolOutput,
    toolInput: p.input !== undefined ? p.input : prev.toolInput,
    toolDurationMs: typeof p.duration_ms === "number" ? p.duration_ms : prev.toolDurationMs,
    toolExitCode: typeof p.exit_code === "number" ? p.exit_code : prev.toolExitCode,
    toolSummary: typeof p.summary === "string" && p.summary ? p.summary : prev.toolSummary,
    toolSubagent: typeof p.subagent === "string" && p.subagent ? p.subagent : prev.toolSubagent,
  };
}

const keyOf = (row: Row, i: number) => (row.seq > 0 ? `s${row.seq}` : `i${i}`);

/**
 * Groups transcript rows into visual blocks:
 * - consecutive (assistant, text) rows join with "" — ACP chunks are deltas;
 * - consecutive (assistant, thought) rows likewise;
 * - consecutive tool_call/status rows cluster into one activity group;
 * - user/error rows break runs.
 * Order-based only: no seq-contiguity assumptions (the event bridge may drop).
 * `indexOffset` shifts the fallback keys of transient (seq 0) rows so callers
 * grouping slices of a larger row array (buildTranscript) keep keys unique.
 */
export function groupRows(rows: Row[], indexOffset = 0): Group[] {
  const groups: Group[] = [];
  rows.forEach((row, i) => {
    const key = keyOf(row, i + indexOffset);
    if (row.role === "user") {
      if (row.text.trim() || row.attachments.length > 0) groups.push({ type: "user", key, text: row.text, attachments: row.attachments });
      return;
    }
    if (row.blockType === "error") {
      groups.push({ type: "error", key, text: row.text });
      return;
    }
    if (row.blockType === "notice") {
      groups.push({ type: "notice", key, text: row.text });
      return;
    }
    // Group-chat rows (worker/orchestrator speaker set) render as their own
    // labeled bubble — one per row, never coalesced with the agent-turn
    // markdown run or with each other, so per-worker bubbles stay distinct.
    // Tool calls dispatched by a sub-agent keep flowing through the normal
    // activity cluster below (their subagent label already renders there).
    if (row.speaker && row.blockType !== "tool_call") {
      groups.push({ type: "speaker", key, speaker: row.speaker, markdown: row.text, blockType: row.blockType, taskId: row.taskId });
      return;
    }
    if (row.blockType === "tool_call" || row.blockType === "status") {
      const item: ActivityItem =
        row.blockType === "tool_call"
          ? {
              type: "tool",
              key,
              name: row.toolName ?? "Tool",
              kind: row.toolKind,
              status: row.toolStatus,
              output: row.toolOutput,
              path: row.toolPath,
              input: row.toolInput,
              durationMs: row.toolDurationMs,
              exitCode: row.toolExitCode,
              summary: row.toolSummary,
              subagent: row.toolSubagent,
            }
          : { type: "status", key, text: row.text };
      if (item.type === "status" && !item.text.trim()) return;
      const last = groups[groups.length - 1];
      if (last?.type === "activity") last.items.push(item);
      else groups.push({ type: "activity", key, items: [item] });
      return;
    }
    // text | thought | forward-compat unknown block types.
    const type = row.blockType === "thought" ? ("thought" as const) : ("agent" as const);
    const last = groups[groups.length - 1];
    if (last && last.type === type) last.markdown += row.text;
    else groups.push({ type, key, markdown: row.text });
  });
  // Whitespace-only chunks stay inside runs (they are paragraph separators),
  // but a run that never got visible content is dropped entirely.
  return groups.filter((g) => (g.type !== "agent" && g.type !== "thought") || g.markdown.trim().length > 0);
}

/** Last-row minus user-row timestamps; null when either is missing. */
export function turnDurationMs(turnRows: Row[]): number | null {
  const start = turnRows.find((r) => r.role === "user")?.createdAt ?? null;
  const end = turnRows[turnRows.length - 1]?.createdAt ?? null;
  return start !== null && end !== null && end >= start ? end - start : null;
}

/** "36s" under a minute, then "3m 59s". Null → "". */
export function formatTurnDuration(ms: number | null): string {
  if (ms === null) return "";
  const secs = Math.max(0, Math.round(ms / 1000));
  if (secs < 60) return `${secs}s`;
  return `${Math.floor(secs / 60)}m ${String(secs % 60).padStart(2, "0")}s`;
}

/** First line of a string, hard-capped for a card header. */
function headerLine(s: string): string {
  const line = s.trim().split("\n", 1)[0] ?? "";
  return line.length > 120 ? `${line.slice(0, 117)}…` : line;
}

/**
 * One-line input summary for a tool card header, keyed on the INPUT SHAPE so
 * native, ACP and MCP tools all work: command → "$ cmd", pattern → the
 * pattern, file tools → target path, url/query → that string, anything else →
 * compact JSON. Null when there is nothing meaningful to show.
 */
export function toolInputSummary(input: unknown, path: string | null): string | null {
  const i = (input && typeof input === "object" && !Array.isArray(input) ? input : {}) as Record<string, unknown>;
  if (typeof i.command === "string" && i.command.trim()) return `$ ${headerLine(i.command)}`;
  if (typeof i.pattern === "string" && i.pattern.trim()) return headerLine(i.pattern);
  if (path) return path;
  if (typeof i.url === "string" && i.url.trim()) return headerLine(i.url);
  if (typeof i.query === "string" && i.query.trim()) return headerLine(i.query);
  if (Object.keys(i).length === 0) return null;
  return headerLine(JSON.stringify(i));
}

/**
 * Header parts for a tool card. `summary` display extras (todo/task/memory)
 * win over the derived input summary. ACP rows use the adapter's human title
 * as `name`, which may already embed the command/path — the detail is dropped
 * when the title already contains it, so headers never double-print.
 */
export function toolCardHeader(item: { name: string; input: unknown; path: string | null; summary: string | null }): {
  title: string;
  detail: string | null;
} {
  const raw = item.summary ?? toolInputSummary(item.input, item.path);
  if (!raw) return { title: item.name, detail: null };
  const core = raw.replace(/^\$\s*/, "");
  return { title: item.name, detail: core && item.name.includes(core) ? null : raw };
}

/** "312ms" under a second, "1.4s" under ten, then the turn format ("36s", "3m 59s"). */
export function formatToolDuration(ms: number | null): string {
  if (ms === null) return "";
  if (ms < 1000) return `${Math.max(0, Math.round(ms))}ms`;
  if (ms < 10_000) return `${(ms / 1000).toFixed(1)}s`;
  return formatTurnDuration(ms);
}

/**
 * Live-turn startup can stream backend status rows (worktree/tool setup)
 * before the durable user-message row is persisted, producing a leading
 * `activity` group ahead of the turn's `user` group. Moves that leading run
 * of `activity` groups to sit right after the `user` group so the user's own
 * message always renders first. No-op when the shape doesn't match (e.g. the
 * user group is already first, or something other than `activity` precedes
 * it).
 */
function placeLeadingLiveActivityAfterUser(groups: Group[]): Group[] {
  const firstUser = groups.findIndex((group) => group.type === "user");
  if (firstUser <= 0) return groups;
  const leading = groups.slice(0, firstUser);
  if (!leading.every((group) => group.type === "activity")) return groups;
  const user = groups[firstUser];
  if (user?.type !== "user") return groups;
  return [user, ...leading, ...groups.slice(firstUser + 1)];
}

/**
 * Splits rows into user turns and collapses each COMPLETED turn's activity
 * (thought/tool/status groups) into one summary block placed where the first
 * collapsed group appeared. Agent text and errors stay inline; the last agent
 * text of a completed turn is flagged `turnEnd` (action bar anchor). The last
 * turn stays uncollapsed while `running` so live activity streams.
 */
export function buildTranscript(rows: Row[], running: boolean): TurnBlock[] {
  const turns: Row[][] = [];
  let cur: Row[] = [];
  for (const r of rows) {
    if (r.role === "user" && cur.length > 0) {
      turns.push(cur);
      cur = [];
    }
    cur.push(r);
  }
  if (cur.length > 0) turns.push(cur);

  // Startup can persist backend status rows (worktree/tool setup) before the
  // durable first user message lands, so the very first "turn" may have no
  // leading user row (every later turn always starts at the user row that
  // split it). When that orphaned turn directly precedes the live final
  // turn, fold it into the live turn so grouping/placement below treats them
  // as one live turn instead of an already-"completed" turn.
  if (running && turns.length === 2 && !turns[0].some((r) => r.role === "user")) {
    turns[1] = [...turns[0], ...turns[1]];
    turns.shift();
  }

  const out: TurnBlock[] = [];
  let rowOffset = 0;
  turns.forEach((turnRows, t) => {
    // Absolute row offset keeps transient (seq 0) fallback keys globally unique
    // even though each turn slice is grouped independently.
    const groups = groupRows(turnRows, rowOffset);
    rowOffset += turnRows.length;
    const live = running && t === turns.length - 1;
    if (live) {
      out.push(...placeLeadingLiveActivityAfterUser(groups));
      return;
    }
    const agentGroups = groups.filter((g) => g.type === "agent");
    const lastAgent = agentGroups[agentGroups.length - 1];
    const collapsible = groups.filter((g) => g.type === "activity" || g.type === "thought");
    for (const g of groups) {
      if (g.type === "activity" || g.type === "thought") {
        if (g === collapsible[0]) {
          out.push({
            type: "summary",
            key: `sum-${g.key}`,
            groups: collapsible,
            durationMs: turnDurationMs(turnRows),
            editCards: editCardsForGroups(collapsible),
          });
        }
      } else if (g === lastAgent && g.type === "agent") {
        out.push({ ...g, turnEnd: true });
      } else {
        out.push(g);
      }
    }
  });
  return out;
}

/**
 * While a markdown buffer is still streaming, an unterminated ``` fence would
 * swallow the rest of the turn as a paragraph. Appends a closing fence when a
 * line-start fence count is odd. Only used for the live tail group.
 */
export function closeDanglingFence(md: string): string {
  const fences = md.match(/^(```|~~~)/gm)?.length ?? 0;
  return fences % 2 === 1 ? `${md}\n\`\`\`` : md;
}
