import { Bot, RotateCcw } from "lucide-react";
import { Button, SettingsCard, SettingsCardHeader, SettingsCardHint, SettingsCardRow, SettingsCardTitle } from "@ryuzi/ui";
import type { CuratorRun, CuratorStatus } from "@/bindings";
import { canRollback, formatRelativeTime } from "@/store-learning";

const STATUS_LABEL: Record<string, string> = { running: "Running", ok: "OK", error: "Error" };

export function CuratorCard({
  status,
  rollingBack,
  onRollback,
}: {
  status: CuratorStatus | null;
  rollingBack: string | null;
  onRollback: (runId: string) => void;
}) {
  const recent = status?.recent ?? [];
  return (
    <SettingsCard>
      <SettingsCardHeader>
        <Bot aria-hidden size={14} strokeWidth={2} className="text-muted-foreground" />
        <SettingsCardTitle>Curator</SettingsCardTitle>
        <span className="ml-auto text-xs text-muted-foreground">
          {status?.lastRunAt != null ? `Last swept ${formatRelativeTime(status.lastRunAt)}` : "Never run"}
        </span>
      </SettingsCardHeader>
      {recent.length === 0 ? (
        <div className="px-[18px] py-6 text-center text-[12.5px] text-muted-foreground">No curator runs yet.</div>
      ) : (
        recent.map((run) => <CuratorRunRow key={run.id} run={run} busy={rollingBack === run.id} onRollback={() => onRollback(run.id)} />)
      )}
    </SettingsCard>
  );
}

function CuratorRunRow({ run, busy, onRollback }: { run: CuratorRun; busy: boolean; onRollback: () => void }) {
  const enabled = canRollback(run);
  return (
    <SettingsCardRow className="items-start">
      <div className="min-w-0 flex-1">
        <div className="flex items-center gap-2 text-[12.5px]">
          <span className="font-medium">{STATUS_LABEL[run.status] ?? run.status}</span>
          <span className="text-muted-foreground">· {formatRelativeTime(run.startedAt)}</span>
          <span className="text-muted-foreground">· {run.transitioned} transitioned</span>
          {run.consolidated && <span className="text-muted-foreground">· consolidated</span>}
        </div>
        {run.error && <SettingsCardHint>{run.error}</SettingsCardHint>}
      </div>
      {/* Rollback stays honest about today's reality: `snapshotPath` is only
          ever set by the opt-in consolidation pass, which hasn't shipped yet
          (Task-12 resolution #2), so this control is disabled for every run
          right now. The `title` sits on the wrapping span, not the disabled
          Button, so the tooltip still shows on hover (a disabled native
          control has pointer-events:none and would otherwise swallow it). */}
      <span title={enabled ? "Roll back to the pre-mutation snapshot" : "Rollback available after a consolidation run"}>
        <Button type="button" variant="outline" size="sm" disabled={!enabled || busy} onClick={onRollback}>
          <RotateCcw aria-hidden size={12} strokeWidth={2} />
          {busy ? "Rolling back…" : "Rollback"}
        </Button>
      </span>
    </SettingsCardRow>
  );
}
