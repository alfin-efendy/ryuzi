import { ScrollText } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import {
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
} from "@ryuzi/ui";
import { commands, type AuditRow } from "@/bindings";
import { formatRelativeTime } from "@/store-learning";

const AUDIT_FEED_LIMIT = 100;

// Settings → App-control audit: a read-only feed of the app-control
// mutations Task 7's `AppControl` facade records on every write (origin,
// tool, action, decision) — surfaced here purely for visibility, with no
// create/edit/delete affordance.
export function AuditCard() {
  const [rows, setRows] = useState<AuditRow[]>([]);

  const load = useCallback(async () => {
    const res = await commands.listAudit(AUDIT_FEED_LIMIT);
    if (res.status === "ok") setRows(res.data);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  return (
    <Card className="mt-3">
      <CardHeader>
        <ScrollText aria-hidden size={14} strokeWidth={2} className="text-muted-foreground" />
        <CardTitle>App-control audit</CardTitle>
      </CardHeader>

      {rows.length === 0 ? (
        <div className="px-[18px] py-3 text-[13px] text-muted-foreground">No app-control activity yet.</div>
      ) : (
        rows.map((r) => (
          <CardRow key={r.id}>
            <span className="font-mono text-[11px] text-muted-foreground">{r.origin}</span>
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13px] font-medium">{r.tool}</div>
              <div className="truncate text-[12px] text-muted-foreground">{r.action}</div>
            </div>
            <span className="text-[12px] text-muted-foreground">{formatRelativeTime(r.at)}</span>
          </CardRow>
        ))
      )}
    </Card>
  );
}
