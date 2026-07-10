import { useState } from "react";
import { Button, MenuPanel } from "@ryuzi/ui";
import { useStore } from "@/store";
import { ContextRing } from "./ContextRing";

function fmtUsd(usd: number): string {
  if (usd <= 0) return "—";
  if (usd < 0.01) return "<$0.01";
  return `$${usd.toFixed(2)}`;
}

const fmtTokens = (n: number) => n.toLocaleString();

/** Composer trigger: context-usage ring + cost popover, replacing the old
 *  "% context left" text. Reads `contextUsage`/`sessionCost` itself so
 *  callers only need the session pk. */
export function SessionCostPanel({ sessionPk }: { sessionPk: string }) {
  const usage = useStore((s) => s.contextUsage[sessionPk]);
  const cost = useStore((s) => s.sessionCost[sessionPk]);
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
        <MenuPanel onClose={() => setOpen(false)} className="bottom-9 right-0 w-[260px]">
          <div className="px-2 py-1.5 text-[11px] tabular-nums text-muted-foreground">
            <div className="mb-1 font-medium text-foreground">Context</div>
            <div className="flex justify-between">
              <span>Active</span>
              <span>{fmtTokens(usage.activeTokens)}</span>
            </div>
            <div className="flex justify-between">
              <span>Usable window</span>
              <span>{fmtTokens(usage.usableWindow)}</span>
            </div>
            <div className="flex justify-between">
              <span>Full window</span>
              <span>{fmtTokens(usage.contextWindow)}</span>
            </div>
            {usage.cacheReadTokens > 0 && (
              <div className="flex justify-between">
                <span>Cache reads</span>
                <span>{fmtTokens(usage.cacheReadTokens)}</span>
              </div>
            )}
          </div>
          <div className="my-1 border-t border-border" />
          <div className="px-2 py-1.5 text-[11px] tabular-nums">
            <div className="mb-1 font-medium text-foreground">Cost</div>
            {cost && cost.models.length > 0 ? (
              cost.models.map((m) => (
                <div key={m.model} className="mb-1">
                  <div className="flex justify-between text-foreground">
                    <span className="truncate">{m.model}</span>
                    <span>{fmtUsd(m.usd)}</span>
                  </div>
                  <div className="text-muted-foreground">
                    {fmtTokens(m.input)} in · {fmtTokens(m.output)} out · {fmtTokens(m.cacheRead + m.cacheCreation)} cache
                  </div>
                </div>
              ))
            ) : (
              <div className="flex justify-between text-muted-foreground">
                <span>Total</span>
                <span>—</span>
              </div>
            )}
            {/* Only worth a separate total row once there's more than one
             * model to sum — with a single model it would just repeat the
             * row above. */}
            {cost && cost.models.length > 1 && (
              <div className="mt-1 flex justify-between border-t border-border pt-1 font-medium text-foreground">
                <span>Total</span>
                <span>{fmtUsd(cost.totalUsd)}</span>
              </div>
            )}
          </div>
        </MenuPanel>
      )}
    </div>
  );
}
