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
};

export type ActivityItem =
  | { type: "tool"; key: string; name: string; kind: string | null; status: string | null; output: string | null }
  | { type: "status"; key: string; text: string };

export type Group =
  | { type: "user"; key: string; text: string }
  | { type: "agent"; key: string; markdown: string }
  | { type: "thought"; key: string; markdown: string }
  | { type: "activity"; key: string; items: ActivityItem[] }
  | { type: "error"; key: string; text: string };

const keyOf = (row: Row, i: number) => (row.seq > 0 ? `s${row.seq}` : `i${i}`);

/**
 * Groups transcript rows into visual blocks:
 * - consecutive (assistant, text) rows join with "" — ACP chunks are deltas;
 * - consecutive (assistant, thought) rows likewise;
 * - consecutive tool_call/status rows cluster into one activity group;
 * - user/error rows break runs.
 * Order-based only: no seq-contiguity assumptions (the event bridge may drop).
 */
export function groupRows(rows: Row[]): Group[] {
  const groups: Group[] = [];
  rows.forEach((row, i) => {
    const key = keyOf(row, i);
    if (row.role === "user") {
      if (row.text.trim()) groups.push({ type: "user", key, text: row.text });
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

/**
 * While a markdown buffer is still streaming, an unterminated ``` fence would
 * swallow the rest of the turn as a paragraph. Appends a closing fence when a
 * line-start fence count is odd. Only used for the live tail group.
 */
export function closeDanglingFence(md: string): string {
  const fences = md.match(/^(```|~~~)/gm)?.length ?? 0;
  return fences % 2 === 1 ? `${md}\n\`\`\`` : md;
}
