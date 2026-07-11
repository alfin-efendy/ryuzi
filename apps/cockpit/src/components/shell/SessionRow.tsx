import type { ReactNode } from "react";
import { Badge, Button } from "@ryuzi/ui";
import { Archive, Pin } from "lucide-react";
import type { UiSession } from "@/lib/session-key";
import { StatusDot, TreeGuide } from "@/components/common/bits";
import { statusMeta } from "@/lib/status";
import { sessionTitle } from "@/lib/sidebar";

export type SessionRowProps = {
  session: UiSession;
  isActive: boolean;
  isPinned: boolean;
  unread: boolean;
  isArchived: boolean;
  hasTail: boolean;
  archiveDisabled: boolean;
  /** Non-null for sessions on a non-local runner — rendered as a small chip
   *  next to the title so multi-runner sessions are distinguishable. */
  runnerLabel?: string | null;
  onOpen: () => void;
  onTogglePin: () => void;
  onToggleArchive: () => void;
  /** Optional drag handle rendered after the tree guide (sortable variant). */
  dragHandle?: ReactNode;
};

export function SessionRow({
  session,
  isActive,
  isPinned,
  unread,
  isArchived,
  hasTail,
  archiveDisabled,
  runnerLabel,
  onOpen,
  onTogglePin,
  onToggleArchive,
  dragHandle,
}: SessionRowProps) {
  const m = statusMeta(session.status);
  return (
    <div className={`group flex min-h-7 items-stretch text-sidebar-foreground ${isArchived ? "opacity-55" : ""}`}>
      <TreeGuide tail={hasTail} reach={3} />
      {dragHandle}
      <span
        className={`my-px flex min-w-0 flex-1 items-center gap-2 rounded-md py-[5px] pl-[7px] pr-1.5 hover:bg-sidebar-accent ${isActive ? "bg-sidebar-accent" : ""}`}
      >
        <Button
          type="button"
          variant="ghost"
          onClick={onOpen}
          className="h-auto min-w-0 flex-1 justify-start gap-2 p-0 text-left text-sidebar-foreground hover:bg-transparent hover:text-sidebar-foreground dark:hover:bg-transparent"
        >
          <StatusDot color={m.color} pulse={m.pulse} />
          <span className={`min-w-0 flex-1 truncate ${unread ? "font-semibold text-foreground" : ""}`}>{sessionTitle(session)}</span>
          {runnerLabel && (
            <Badge variant="secondary" className="h-4 shrink-0 px-1 text-[9.5px] font-medium">
              {runnerLabel}
            </Badge>
          )}
          <span aria-hidden className="flex w-2 shrink-0 items-center justify-center">
            {unread && <span data-testid={`unread-dot-${session.sessionPk}`} className="size-1.5 rounded-full bg-primary" />}
          </span>
          {unread && <span className="sr-only">unread</span>}
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          title={isPinned ? "Unpin" : "Pin"}
          className={`size-[22px] shrink-0 rounded-sm ${isPinned ? "flex text-foreground" : "hidden text-muted-foreground group-hover:flex"}`}
          onClick={onTogglePin}
        >
          <Pin aria-hidden size={12} strokeWidth={2} fill={isPinned ? "currentColor" : "none"} />
        </Button>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          title={isArchived ? "Restore" : "Archive — ends the session and removes its worktree"}
          disabled={archiveDisabled}
          className="hidden size-[22px] shrink-0 rounded-sm text-muted-foreground disabled:opacity-40 group-hover:flex"
          onClick={onToggleArchive}
        >
          <Archive aria-hidden size={12} strokeWidth={2} />
        </Button>
      </span>
    </div>
  );
}
