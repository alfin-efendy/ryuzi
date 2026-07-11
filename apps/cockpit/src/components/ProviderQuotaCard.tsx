import { RefreshCw, RotateCcw } from "lucide-react";
import type { ProviderQuotaCapability, ProviderQuotaInfo, QuotaWindowInfo } from "@/bindings";
import { quotaColor } from "@/constants";
import { QuotaTrack } from "@/components/common/bits";
import {
  Button,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
} from "@ryuzi/ui";

const percent = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 });

function formatPercent(value: number) {
  return `${percent.format(value)}%`;
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

function quotaHint(capability: ProviderQuotaCapability) {
  if (capability === "claude") return "Live quota from Claude subscription";
  return "Live quota from ChatGPT Codex subscription";
}

function QuotaRow({ quota }: { quota: QuotaWindowInfo }) {
  return (
    <CardRow className="flex-col items-stretch gap-2">
      <div className="flex items-center justify-between gap-3">
        <span className="min-w-0 truncate text-[13px] font-medium">{quota.label}</span>
        <span className="shrink-0 text-xs text-muted-foreground">{formatPercent(quota.remainingPercentage)} left</span>
      </div>
      <QuotaTrack pct={quota.usedPercentage} color={quotaColor(quota.usedPercentage)} height={5} />
      <div className="flex items-center justify-between gap-3 text-[11.5px] text-muted-foreground">
        <span>{formatPercent(quota.usedPercentage)} used</span>
        <span className="truncate text-right">Resets {formatReset(quota.resetAt)}</span>
      </div>
    </CardRow>
  );
}

export function ProviderQuotaCard({
  capability,
  quota,
  loading,
  resetting,
  onRefresh,
  onResetCredit,
}: {
  capability: ProviderQuotaCapability;
  quota: ProviderQuotaInfo | null;
  loading: boolean;
  resetting: boolean;
  onRefresh: () => void;
  onResetCredit?: () => void;
}) {
  const credits = quota?.resetCredits;
  const canShowReset = capability === "codex" && !!onResetCredit;
  const availableCredits = credits?.availableCount ?? 0;

  return (
    <Card className="mt-3">
      <CardHeader className="justify-between">
        <div className="min-w-0">
          <CardTitle>Provider quota</CardTitle>
          <CardHint>{quotaHint(capability)}</CardHint>
        </div>
        <div className="flex shrink-0 items-center gap-2">
          {canShowReset && (
            <Button variant="outline" onClick={onResetCredit} disabled={resetting || (!!credits && availableCredits <= 0)}>
              <RotateCcw aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
              {resetting ? "Resetting…" : "Reset credit"}
            </Button>
          )}
          <Button
            variant="outline"
            size="icon"
            title="Refresh provider quota"
            aria-label="Refresh provider quota"
            onClick={onRefresh}
            disabled={loading}
          >
            <RefreshCw aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
          </Button>
        </div>
      </CardHeader>

      {quota?.plan && (
        <CardRow>
          <span className="w-28 shrink-0 text-[13px] font-medium">Plan</span>
          <span className="flex-1 text-[13px] text-muted-foreground">{quota.plan}</span>
        </CardRow>
      )}

      {canShowReset && (
        <CardRow>
          <span className="w-28 shrink-0 text-[13px] font-medium">Reset credits</span>
          <span className="flex-1 text-[13px] text-muted-foreground">{availableCredits} available</span>
        </CardRow>
      )}

      {quota?.message && (
        <CardRow>
          <span className="text-[13px] text-muted-foreground">{quota.message}</span>
        </CardRow>
      )}

      {quota?.quotas.map((item) => (
        <QuotaRow key={item.label} quota={item} />
      ))}

      {!quota && (
        <div className="px-[18px] py-3 text-[13px] text-muted-foreground">{loading ? "Loading quota…" : "No live quota loaded yet."}</div>
      )}
    </Card>
  );
}
