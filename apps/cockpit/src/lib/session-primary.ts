import type { AgentIdentitySnapshot, AgentSummaryInfo } from "@/bindings";

/** The primary identity is captured when a session starts and never follows
 * mutable agent profile edits. Legacy sessions intentionally have no owner. */
export function sessionPrimaryLabel(
  snapshot: AgentIdentitySnapshot | null,
  registry: AgentSummaryInfo[] | null | undefined,
): string {
  if (snapshot === null) return "Legacy agent";
  return registry?.some((agent) => agent.id === snapshot.id) === false ? `${snapshot.name} (Deleted)` : snapshot.name;
}

/** Sessions without a captured primary owner are transcript-only history. */
export function sessionIsReadOnly(snapshot: AgentIdentitySnapshot | null): boolean {
  return snapshot === null;
}
