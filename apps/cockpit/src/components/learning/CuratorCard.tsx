import { Bot, RotateCcw } from "lucide-react";
import { Button, SettingsCard, SettingsCardHint, SettingsCardRow } from "@ryuzi/ui";
import type { CuratorHistorySnapshotInfo, CuratorStateInfo } from "@/bindings";

export function CuratorCard({
  curator,
  history,
  rollingBack,
  onRollback,
}: {
  curator: CuratorStateInfo;
  history: CuratorHistorySnapshotInfo[];
  rollingBack: string | null;
  onRollback: (snapshot: CuratorHistorySnapshotInfo) => void;
}) {
  return (
    <SettingsCard>
      <SettingsCardRow className="items-start">
        <Bot aria-hidden size={14} className="mt-0.5 text-muted-foreground" />
        <div className="min-w-0 flex-1">
          <div className="text-xs font-medium">{curator.concept?.title ?? "No curator state yet"}</div>
          {curator.concept ? <SettingsCardHint>{curator.concept.description || curator.concept.body}</SettingsCardHint> : null}
        </div>
      </SettingsCardRow>
      {history.map((snapshot) => (
        <SettingsCardRow key={snapshot.snapshotId} className="items-start">
          <div className="min-w-0 flex-1">
            <div className="text-xs font-medium">{snapshot.concept.title}</div>
            <SettingsCardHint>{snapshot.snapshotId}</SettingsCardHint>
          </div>
          <Button
            type="button"
            variant="outline"
            size="sm"
            aria-label={`Rollback ${snapshot.concept.title}`}
            disabled={rollingBack !== null}
            onClick={() => onRollback(snapshot)}
          >
            <RotateCcw aria-hidden size={12} />
            {rollingBack === snapshot.snapshotId ? "Restoring…" : "Rollback"}
          </Button>
        </SettingsCardRow>
      ))}
    </SettingsCard>
  );
}
