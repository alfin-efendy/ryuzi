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
};

export type RowAttachment = { name: string; path: string; contentType: string | null; size: number };

export type ActivityItem =
  | { type: "tool"; key: string; name: string; kind: string | null; status: string | null; output: string | null }
  | { type: "status"; key: string; text: string };

export type Group =
  | { type: "user"; key: string; text: string; attachments: RowAttachment[] }
  | { type: "agent"; key: string; markdown: string; turnEnd?: boolean }
  | { type: "thought"; key: string; markdown: string }
  | { type: "activity"; key: string; items: ActivityItem[] }
  | { type: "error"; key: string; text: string };

export type TurnBlock = Group | { type: "summary"; key: string; groups: Group[]; durationMs: number | null };

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
    if (row.blockType === "tool_call" || row.blockType === "status") {
      const item: ActivityItem =
        row.blockType === "tool_call"
          ? { type: "tool", key, name: row.toolName ?? "Tool", kind: row.toolKind, status: row.toolStatus, output: row.toolOutput }
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

  const out: TurnBlock[] = [];
  let rowOffset = 0;
  turns.forEach((turnRows, t) => {
    // Absolute row offset keeps transient (seq 0) fallback keys globally unique
    // even though each turn slice is grouped independently.
    const groups = groupRows(turnRows, rowOffset);
    rowOffset += turnRows.length;
    const live = running && t === turns.length - 1;
    if (live) {
      out.push(...groups);
      return;
    }
    const agentGroups = groups.filter((g) => g.type === "agent");
    const lastAgent = agentGroups[agentGroups.length - 1];
    const collapsible = groups.filter((g) => g.type === "activity" || g.type === "thought");
    for (const g of groups) {
      if (g.type === "activity" || g.type === "thought") {
        if (g === collapsible[0]) {
          out.push({ type: "summary", key: `sum-${g.key}`, groups: collapsible, durationMs: turnDurationMs(turnRows) });
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
