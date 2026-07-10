import type { CoreEvent, Session } from "@/bindings";
import { isUnreadVisible, sessionTitle } from "@/lib/sidebar";

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

export const SETTLE_MS = 3000;

export type NotifierDeps = {
  sendNotification: (o: { title: string; body: string }) => void;
  setBadgeCount: (n: number | undefined) => void;
  ensurePermission: () => Promise<boolean>;
  isEnabled: () => boolean;
  /** Schedule `fn` after `ms`; returns a cancel function. */
  schedule: (fn: () => void, ms: number) => () => void;
};

export type Notifier = {
  handle(intent: NonNullable<NotifyIntent>, session: Session | undefined): void;
  cancelSettle(sessionPk: string): void;
  cancelAllSettles(): void;
  updateBadge(count: number): void;
};

/** Title/body for a notification. Title is the session title; body states the
 *  kind. */
export function notificationText(
  intent: NonNullable<NotifyIntent>,
  session: Session | undefined,
): { title: string; body: string } {
  const title = session ? sessionTitle(session) : "Session";
  const body =
    intent.kind === "approval"
      ? `Needs approval: ${intent.detail ?? "a tool"}`
      : intent.kind === "error"
        ? "Turn errored"
        : "Turn finished";
  return { title, body };
}

export function createNotifier(deps: NotifierDeps): Notifier {
  const settles = new Map<string, () => void>();

  const cancelSettle = (sessionPk: string) => {
    const cancel = settles.get(sessionPk);
    if (cancel) {
      cancel();
      settles.delete(sessionPk);
    }
  };

  const send = (intent: NonNullable<NotifyIntent>, session: Session | undefined) => {
    if (!deps.isEnabled()) return;
    void deps.ensurePermission().then((ok) => {
      if (ok) deps.sendNotification(notificationText(intent, session));
    });
  };

  return {
    handle(intent, session) {
      // Any new event for a session supersedes its pending "finished" settle.
      cancelSettle(intent.sessionPk);
      if (!deps.isEnabled()) return;
      if (intent.settle) {
        const cancel = deps.schedule(() => {
          settles.delete(intent.sessionPk);
          send(intent, session);
        }, SETTLE_MS);
        settles.set(intent.sessionPk, cancel);
      } else {
        send(intent, session);
      }
    },
    cancelSettle,
    cancelAllSettles() {
      for (const cancel of settles.values()) cancel();
      settles.clear();
    },
    updateBadge(count) {
      deps.setBadgeCount(count || undefined);
    },
  };
}
