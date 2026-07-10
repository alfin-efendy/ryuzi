import { Inbox } from "lucide-react";
import { Button } from "@ryuzi/ui";
import { useStore } from "@/store";
import { useNav } from "@/store-nav";
import { ApprovalCard } from "@/components/approval/ApprovalCard";

/** Cross-session queue of everything waiting on the user: newest first,
 *  every card fully rendered and interactive (no expand/collapse — they're
 *  just stacked in a list). */
export function InboxView() {
  const pending = useStore((s) => s.pendingApprovals);
  const setFocused = useStore((s) => s.setFocused);
  const navigate = useNav((s) => s.navigate);
  const items = [...pending].reverse();

  return (
    <div className="flex min-h-0 flex-1 flex-col overflow-y-auto px-6 py-5">
      <div className="mx-auto w-full max-w-[760px]">
        <div className="mb-4 flex items-center gap-2">
          <Inbox size={16} className="text-muted-foreground" />
          <h1 className="text-[15px] font-semibold">Inbox</h1>
          <span className="text-xs text-muted-foreground">{pending.length} pending</span>
        </div>
        {items.length === 0 ? (
          <div className="rounded-xl border border-dashed border-border px-6 py-10 text-center text-sm text-muted-foreground">
            No pending approvals. Agents that need your input will show up here.
          </div>
        ) : (
          <div className="space-y-3">
            {items.map((a) => (
              <div key={a.requestId} className="space-y-1">
                <ApprovalCard approval={a} showSession />
                <div className="flex justify-end">
                  <Button
                    size="sm"
                    variant="ghost"
                    className="text-xs text-muted-foreground"
                    onClick={() => {
                      setFocused(a.sessionPk);
                      navigate({ kind: "session" });
                    }}
                  >
                    Open session
                  </Button>
                </div>
              </div>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
