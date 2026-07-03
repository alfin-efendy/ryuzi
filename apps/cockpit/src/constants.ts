// Pure UI copy and presentation constants. Anything stateful lives in the
// domain stores backed by real IPC — nothing here is data.

export type UiPermMode = "plan" | "ask" | "edit" | "full";

export const PERM_MODES: { id: UiPermMode; label: string; desc: string }[] = [
  { id: "plan", label: "Plan", desc: "Proposes a plan first; every action needs approval." },
  { id: "ask", label: "Ask", desc: "Asks before edits and shell commands." },
  { id: "edit", label: "Edit", desc: "Edits files freely, asks before shell commands." },
  { id: "full", label: "Full", desc: "Full access — no approval prompts." },
];

export type RotationStrategy = "priority" | "roundrobin" | "sticky";

export const ROTATION_STRATEGIES: { id: RotationStrategy; label: string; desc: string; pill: string }[] = [
  {
    id: "priority",
    label: "Priority order",
    desc: "The top account serves requests; rotate down the list when a quota runs out.",
    pill: "Priority rotation",
  },
  {
    id: "roundrobin",
    label: "Round-robin",
    desc: "Spread requests evenly across every enabled account.",
    pill: "Round-robin",
  },
  {
    id: "sticky",
    label: "Sticky sessions",
    desc: "Each session keeps its account for cache reuse; new sessions pick the freest one.",
    pill: "Sticky sessions",
  },
];

export type GatewayFsMode = "full" | "projects" | "read";

export const GW_FS_MODES: { id: GatewayFsMode; label: string; desc: string }[] = [
  { id: "full", label: "Full", desc: "Agents may read and write anywhere the daemon user can." },
  { id: "projects", label: "Projects only", desc: "Agents are sandboxed to the connected project folders." },
  { id: "read", label: "Read-only", desc: "Agents can inspect files but never write outside a worktree." },
];

// Command entries surfaced by the ⌘K palette.
export const SEARCH_COMMANDS: { id: string; label: string; keywords: string }[] = [
  { id: "new-session", label: "New session", keywords: "new session start compose" },
  { id: "toggle-terminal", label: "Toggle terminal panel", keywords: "terminal bottom panel shell" },
  { id: "toggle-right", label: "Toggle right panel", keywords: "review files right panel" },
  { id: "gateways", label: "Manage gateways", keywords: "gateway workspace ssh daemon connect" },
  { id: "providers", label: "Open providers", keywords: "provider quota account claude openai" },
  { id: "scheduler", label: "New scheduled job", keywords: "schedule cron job recurring" },
  { id: "settings", label: "Open settings", keywords: "settings appearance theme transparency" },
];

export const HOME_SUGGESTIONS = ["Fix the failing e2e suite", "Add rate limiting to the API", "Upgrade to React 20"];

// Quota bars tint by pressure: calm → amber → red as headroom shrinks.
export function quotaColor(pct: number): string {
  if (pct >= 90) return "#EF4444";
  if (pct >= 75) return "#F59E0B";
  return "#22C55E";
}
