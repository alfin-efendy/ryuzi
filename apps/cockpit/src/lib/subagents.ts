import type { Row } from "@/lib/transcript";

export type SubagentSummary = {
  name: string;
  toolCount: number;
  running: boolean;
  lastActivity: number | null;
};

/** Roster of sub-agents used in a session, derived from tool_call rows that
 *  carry a `toolSubagent` label. Grouped by name, counted, running-flagged when
 *  any tool is pending/in_progress, sorted most-recently-active first. Rows
 *  without a sub-agent label (or non-tool_call rows) are ignored. */
export function subagentSummaries(rows: Row[]): SubagentSummary[] {
  const byName = new Map<string, SubagentSummary>();
  for (const r of rows) {
    if (r.blockType !== "tool_call" || !r.toolSubagent) continue;
    const cur = byName.get(r.toolSubagent) ?? { name: r.toolSubagent, toolCount: 0, running: false, lastActivity: null };
    cur.toolCount += 1;
    if (r.toolStatus === "pending" || r.toolStatus === "in_progress") cur.running = true;
    if (r.createdAt != null && (cur.lastActivity == null || r.createdAt > cur.lastActivity)) cur.lastActivity = r.createdAt;
    byName.set(r.toolSubagent, cur);
  }
  return [...byName.values()].sort((a, b) => (b.lastActivity ?? 0) - (a.lastActivity ?? 0));
}
