import { useStore } from "@/store";
import { Button } from "@harness/ui";

export function ApprovalPrompt({ sessionPk }: { sessionPk: string }) {
  const { pendingApprovals, resolveApproval } = useStore();
  const a = pendingApprovals.find((x) => x.sessionPk === sessionPk);
  if (!a) return null;
  return (
    <div className="flex items-center gap-2 border-t border-amber-300 bg-amber-50 p-3 text-sm dark:bg-amber-950/30">
      <span className="flex-1 font-mono">{a.summary}</span>
      <Button size="sm" variant="secondary" onClick={() => resolveApproval(a.requestId, false)}>
        Deny
      </Button>
      <Button size="sm" onClick={() => resolveApproval(a.requestId, true)}>
        Allow
      </Button>
    </div>
  );
}
