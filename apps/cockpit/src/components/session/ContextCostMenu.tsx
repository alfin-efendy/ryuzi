import { MenuPanel } from "@ryuzi/ui";
import type { ModelCost } from "@/bindings";

function fmtUsd(usd: number): string {
  if (usd <= 0) return "—";
  if (usd < 0.01) return "<$0.01";
  return `$${usd.toFixed(2)}`;
}
const fmtTokens = (n: number) => n.toLocaleString();

export type ContextCostUsage = {
  activeTokens: number;
  usableWindow: number;
  contextWindow: number;
  percentLeft: number;
  cacheReadTokens: number;
};

/** The Context + Cost popover body, shared by the session composer and the
 *  sub-agent run header. Positioning comes from `className`. */
export function ContextCostMenu({
  usage,
  cost,
  className,
  onClose,
}: {
  usage: ContextCostUsage;
  cost: { totalUsd: number; models: ModelCost[] } | undefined;
  className?: string;
  onClose: () => void;
}) {
  return (
    <MenuPanel onClose={onClose} className={className}>
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
                <div className="flex justify-between">
                  <span>In</span>
                  <span>{fmtTokens(m.input)}</span>
                </div>
                <div className="flex justify-between">
                  <span>Out</span>
                  <span>{fmtTokens(m.output)}</span>
                </div>
                <div className="flex justify-between">
                  <span>Cache</span>
                  <span>{fmtTokens(m.cacheRead + m.cacheCreation)}</span>
                </div>
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
  );
}
