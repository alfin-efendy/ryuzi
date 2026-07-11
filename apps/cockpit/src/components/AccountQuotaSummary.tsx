import { RefreshCw, RotateCcw } from "lucide-react";
import type { ProviderQuotaCapability, QuotaWindowInfo } from "@/bindings";
import { quotaColor } from "@/constants";
import { useConnectionQuota } from "@/hooks/useConnectionQuota";
import { Button } from "@ryuzi/ui";

export type AccountQuotaSummaryProps = {
  connectionId: string;
  accountName: string;
  capability: ProviderQuotaCapability;
  onRequestReset: (request: { accountName: string; onConfirm: () => Promise<boolean> }) => void;
};

const percent = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });

function formatPercent(value: number) {
  return `${percent.format(value)}%`;
}

function clampPercent(value: number) {
  return Math.max(0, Math.min(100, value));
}

function formatReset(value: string | null) {
  if (!value) return "No reset time";
  const date = new Date(value);
  if (Number.isNaN(date.getTime())) return value;
  return date.toLocaleString(undefined, {
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
  });
}

function QuotaRow({ accountName, quota }: { accountName: string; quota: QuotaWindowInfo }) {
  const used = clampPercent(quota.usedPercentage);
  const label = `${accountName} ${quota.label} quota`;

  return (
    <div className="flex flex-col gap-2 border-t border-border/60 px-4 py-3">
      <div className="flex items-center justify-between gap-3">
        <span className="min-w-0 truncate text-[13px] font-medium">{quota.label}</span>
        <span className="shrink-0 text-xs text-muted-foreground">{formatPercent(quota.remainingPercentage)} left</span>
      </div>
      <div
        aria-label={label}
        aria-valuemax={100}
        aria-valuemin={0}
        aria-valuenow={used}
        className="h-1.5 overflow-hidden rounded-full bg-muted"
        role="progressbar"
      >
        <div className="h-full rounded-full" style={{ width: `${used}%`, background: quotaColor(used) }} />
      </div>
      <div className="flex items-center justify-between gap-3 text-[11.5px] text-muted-foreground">
        <span>{formatPercent(quota.usedPercentage)} used</span>
        <span className="truncate text-right">{quota.resetAt ? `Resets ${formatReset(quota.resetAt)}` : formatReset(null)}</span>
      </div>
    </div>
  );
}

export function AccountQuotaSummary({ connectionId, accountName, capability, onRequestReset }: AccountQuotaSummaryProps) {
  const { state, refresh, resetCredit, resetting } = useConnectionQuota(connectionId, capability);
  const quota = state.quota;
  const unavailable = state.status === "error";
  const refreshName = unavailable ? `Retry quota for ${accountName}` : `Refresh quota for ${accountName}`;

  return (
    <div className="border-t border-border/60">
      <div className="flex items-center justify-between gap-3 px-4 py-3">
        <span className="text-[13px] font-medium">Quota</span>
        <div className="flex shrink-0 items-center gap-2">
          {capability === "codex" && (
            <Button
              aria-label={`Reset credit for ${accountName}`}
              onClick={() => onRequestReset({ accountName, onConfirm: resetCredit })}
              size="sm"
              variant="outline"
              disabled={resetting}
            >
              <RotateCcw aria-hidden data-icon="inline-start" />
              {resetting ? "Resetting…" : "Reset credit"}
            </Button>
          )}
          <Button
            aria-label={refreshName}
            onClick={() => void refresh()}
            size="icon-sm"
            variant="outline"
            disabled={state.status === "loading"}
          >
            <RefreshCw aria-hidden data-icon="inline-start" />
          </Button>
        </div>
      </div>

      {state.status === "loading" && !quota && <div className="px-4 pb-3 text-[13px] text-muted-foreground">Loading quota…</div>}
      {unavailable && (
        <div className="px-4 pb-3 text-[13px] text-muted-foreground">
          Quota unavailable
          <Button className="ml-2" onClick={() => void refresh()} size="sm" variant="ghost">
            Retry
          </Button>
        </div>
      )}
      {quota && (
        <>
          {(quota.plan || quota.message || capability === "codex") && (
            <div className="flex flex-col gap-1 px-4 pb-3 text-[12px] text-muted-foreground">
              {quota.plan && <span>{quota.plan}</span>}
              {quota.message && <span>{quota.message}</span>}
              {capability === "codex" && <span>{quota.resetCredits?.availableCount ?? 0} reset credits available</span>}
            </div>
          )}
          {quota.quotas.map((item) => (
            <QuotaRow accountName={accountName} key={item.label} quota={item} />
          ))}
        </>
      )}
    </div>
  );
}
