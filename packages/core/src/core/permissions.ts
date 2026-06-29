import type { PermMode } from "@harness/protocol";

export type ToolDecision = "allow" | "ask";

export const SAFE_TOOLS = ["Read", "Grep", "Glob", "LS", "NotebookRead", "TodoWrite"] as const;
const EDIT_TOOLS = ["Edit", "Write", "MultiEdit", "NotebookEdit"];

export function resolveToolPolicy(permMode: PermMode, toolName: string): ToolDecision {
  if (permMode === "bypassPermissions") return "allow";
  if ((SAFE_TOOLS as readonly string[]).includes(toolName)) return "allow";
  if (permMode === "acceptEdits" && EDIT_TOOLS.includes(toolName)) return "allow";
  return "ask";
}

/**
 * Whether a clicker may approve a tool. The session starter always may. If NO
 * approver roles are configured, only the starter may approve (safe-by-default).
 * Otherwise the clicker must hold one of the approver roles.
 */
export function canApprove(o: { clickerRoleIds: string[]; approverRoleIds: string[]; isStarter: boolean }): boolean {
  if (o.isStarter) return true;
  if (o.approverRoleIds.length === 0) return false;
  return o.clickerRoleIds.some((r) => o.approverRoleIds.includes(r));
}

/** Split a comma-separated role-id setting into a trimmed, non-empty list. */
export function parseRoleIds(raw: string | undefined): string[] {
  return (raw ?? "")
    .split(",")
    .map((s) => s.trim())
    .filter(Boolean);
}

/**
 * Whether a user holds an admin role. If NO admin roles are configured, everyone
 * is treated as admin (optional gating — preserves the zero-config single-user UX).
 */
export function isAdmin(o: { userRoleIds: string[]; adminRoleIds: string[] }): boolean {
  if (o.adminRoleIds.length === 0) return true;
  return o.userRoleIds.some((r) => o.adminRoleIds.includes(r));
}

/**
 * Clamp a privileged permission mode for non-admins. Only bypassPermissions is
 * gated (it disables all tool approval). Returns the effective mode and whether
 * it was downgraded so the caller can warn the user.
 */
export function gatePermMode(requested: PermMode, isAdminUser: boolean): { mode: PermMode; downgraded: boolean } {
  if (!isAdminUser && requested === "bypassPermissions") return { mode: "default", downgraded: true };
  return { mode: requested, downgraded: false };
}

export function summarizeTool(toolName: string, input: unknown): string {
  const obj = (input ?? {}) as Record<string, unknown>;
  if (toolName === "Bash" && typeof obj.command === "string") return `Bash: ${obj.command.slice(0, 80)}`;
  const target = obj.file_path ?? obj.path ?? obj.pattern;
  return typeof target === "string" ? `${toolName}: ${target}` : toolName;
}
