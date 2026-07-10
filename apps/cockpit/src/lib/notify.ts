import type { CoreEvent, Session } from "@/bindings";
import { isUnreadVisible } from "@/lib/sidebar";

/** Total items needing the user: unread sessions + pending approvals. The
 *  focused session is never counted (isUnreadVisible excludes it). */
export function attentionCount(
  sessions: Session[],
  readAt: Record<string, number>,
  focusedSessionPk: string | null,
  pendingApprovalCount: number,
): number {
  const unread = sessions.filter((s) => isUnreadVisible(s, readAt, focusedSessionPk)).length;
  return unread + pendingApprovalCount;
}

export type NotifyIntent =
  | { sessionPk: string; kind: "finished" | "approval" | "error"; settle: boolean; detail?: string }
  | null;

/** What (if anything) to notify for a CoreEvent. Suppressed entirely while the
 *  window is focused (the in-app unread dot already signals it). */
export function notifyIntentForEvent(
  event: CoreEvent,
  _focusedSessionPk: string | null,
  windowFocused: boolean,
): NotifyIntent {
  if (windowFocused) return null;
  switch (event.kind) {
    case "result":
      return { sessionPk: event.session_pk, kind: "finished", settle: true };
    case "approvalRequested":
      return { sessionPk: event.session_pk, kind: "approval", settle: false, detail: event.tool };
    case "error":
      return { sessionPk: event.session_pk, kind: "error", settle: false };
    default:
      return null;
  }
}
