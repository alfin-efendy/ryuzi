import type { AgentIdentitySnapshot } from "@/bindings";

/** The primary identity is captured when a session starts and never follows
 * mutable agent profile edits. Legacy sessions intentionally have no owner. */
export function sessionPrimaryLabel(snapshot: AgentIdentitySnapshot | null): string {
  return snapshot?.name ?? "Legacy session";
}

/** Sessions without a captured primary owner are transcript-only history. */
export function sessionIsReadOnly(snapshot: AgentIdentitySnapshot | null): boolean {
  return snapshot === null;
}
