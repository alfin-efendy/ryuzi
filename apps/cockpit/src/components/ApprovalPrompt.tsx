import { useStore } from "@/store";
import { Button } from "@ryuzi/ui";

export function ApprovalPrompt({ sessionPk }: { sessionPk: string }) {
  const { pendingApprovals, resolveApproval } = useStore();
  const a = pendingApprovals.find((x) => x.sessionPk === sessionPk);
  if (!a) return null;
  return (
    <div className="px-4 pb-2">
      <div className="mx-auto max-w-[720px] overflow-hidden rounded-xl border border-border bg-card shadow-sm">
        <div className="flex items-center gap-2.5 border-b border-border bg-muted/40 px-3.5 py-2.5">
          <div className="flex h-[26px] w-[26px] items-center justify-center rounded-lg bg-amber-500/15 text-amber-600 dark:text-amber-400">
            <svg aria-hidden="true" width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <path d="M12 9v4m0 4h.01M10.3 3.86 1.82 18a2 2 0 0 0 1.71 3h16.94a2 2 0 0 0 1.71-3L13.7 3.86a2 2 0 0 0-3.42 0z" />
            </svg>
          </div>
          <div className="min-w-0">
            <div className="text-[13px] font-semibold">Approval needed</div>
            <div className="truncate text-[11.5px] text-muted-foreground">{a.tool}</div>
          </div>
        </div>
        <div className="px-3.5 py-3 font-mono text-xs break-words whitespace-pre-wrap">{a.summary}</div>
        <div className="flex justify-end gap-2 border-t border-border bg-muted/40 px-3.5 py-2.5">
          <Button
            size="sm"
            variant="outline"
            onClick={() => resolveApproval(a.requestId, { decision: "rejectOnce", scope: null, payload: null })}
          >
            Deny
          </Button>
          <Button size="sm" onClick={() => resolveApproval(a.requestId, { decision: "allowOnce", scope: null, payload: null })}>
            Allow
          </Button>
        </div>
      </div>
    </div>
  );
}
