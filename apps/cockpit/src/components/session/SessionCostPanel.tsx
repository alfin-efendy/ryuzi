import { useState } from "react";
import { Button } from "@ryuzi/ui";
import { useStore } from "@/store";
import { sessKey } from "@/lib/session-key";
import { ContextCostMenu } from "./ContextCostMenu";
import { ContextRing } from "./ContextRing";

/** Composer trigger: context-usage ring + cost popover, replacing the old
 *  "% context left" text. Reads `contextUsage`/`sessionCost` itself so
 *  callers only need the session pk. */
export function SessionCostPanel({ runnerId, sessionPk }: { runnerId: string; sessionPk: string }) {
  const key = sessKey(runnerId, sessionPk);
  const usage = useStore((s) => s.contextUsage[key]);
  const cost = useStore((s) => s.sessionCost[key]);
  const [open, setOpen] = useState(false);
  if (!usage) return null;

  return (
    <div className="relative w-32 shrink-0 text-right">
      <Button
        variant="ghost"
        size="xs"
        aria-expanded={open}
        aria-label="Context and cost"
        title={`~${usage.activeTokens.toLocaleString()} of ${usage.usableWindow.toLocaleString()} tokens used`}
        onClick={() => setOpen((v) => !v)}
        className="ml-auto h-auto justify-end gap-1.5 p-0 hover:bg-transparent dark:hover:bg-transparent"
      >
        <ContextRing percentLeft={usage.percentLeft} />
      </Button>
      {open && (
        <ContextCostMenu
          onClose={() => setOpen(false)}
          className="bottom-9 right-0 w-[260px]"
          usage={{
            activeTokens: usage.activeTokens,
            usableWindow: usage.usableWindow,
            contextWindow: usage.contextWindow,
            cacheReadTokens: usage.cacheReadTokens,
          }}
          cost={cost}
        />
      )}
    </div>
  );
}
