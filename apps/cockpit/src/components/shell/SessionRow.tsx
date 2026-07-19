import { Badge, Button } from "@ryuzi/ui";
import { Archive, Loader2, Pin } from "lucide-react";
import type { UiSession } from "@/lib/session-key";
import { TreeGuide } from "@/components/common/bits";
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
  /** Flat rows (the Tasks section) omit the tree guide. Default true. */
  showGuide?: boolean;
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
  showGuide = true,
}: SessionRowProps) {
  const m = statusMeta(session.status);
  const running = session.status === "running";
  // The right edge shows live status (running spinner / unread dot) at rest and
  // swaps to the pin/archive actions on hover, so both never fight for space.
  const showStatus = running || unread;
  return (
    <div className={`group flex min-h-7 items-stretch text-sidebar-foreground ${isArchived ? "opacity-55" : ""}`}>
      {showGuide && <TreeGuide tail={hasTail} reach={3} />}
      <span
        className={`my-px flex min-w-0 flex-1 items-center gap-2 rounded-md py-[5px] pl-2 pr-1.5 transition-colors duration-150 ease-out hover:bg-sidebar-accent ${isActive ? "bg-sidebar-accent" : ""}`}
      >
        <Button
          type="button"
          variant="ghost"
          onClick={onOpen}
          className="h-auto min-w-0 flex-1 justify-start gap-2 p-0 text-left text-[13px] text-sidebar-foreground/85 hover:bg-transparent hover:text-sidebar-foreground dark:hover:bg-transparent"
        >
          <span
            className={`min-w-0 flex-1 truncate tracking-[-0.006em] transition-colors duration-150 ${unread ? "font-semibold text-foreground" : "font-normal"}`}
          >
            {sessionTitle(session)}
          </span>
          {runnerLabel && (
            <Badge variant="secondary" className="h-4 shrink-0 px-1 text-[9.5px] font-medium">
              {runnerLabel}
            </Badge>
          )}
          {unread && <span className="sr-only">unread</span>}
        </Button>

        {/* Fixed-width right slot. At rest it shows live status (running spinner
            or unread dot); on hover it reveals the pin/archive actions in the
            same footprint, so the row width never shifts. A pinned row keeps
            its pin visible at rest instead of the status dot. */}
        <span className="relative flex h-[22px] shrink-0 items-center justify-end">
          {showStatus && !isPinned && (
            <span
              aria-hidden
              className="flex w-[22px] items-center justify-center transition-opacity duration-150 group-hover:pointer-events-none group-hover:opacity-0"
            >
              {running ? (
                <Loader2
                  data-testid={`running-spinner-${session.sessionPk}`}
                  className="size-[13px] animate-spin"
                  strokeWidth={2.25}
                  style={{ color: m.color }}
                />
              ) : (
                <span data-testid={`unread-dot-${session.sessionPk}`} className="size-1.5 rounded-full bg-primary" />
              )}
            </span>
          )}
          <span
            className={`flex items-center ${showStatus && !isPinned ? "absolute inset-y-0 right-0 opacity-0 group-hover:opacity-100" : ""}`}
          >
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
        </span>
      </span>
    </div>
  );
}
