import { Trash2 } from "lucide-react";
import { useCallback, useEffect, useState } from "react";
import {
  Badge,
  Button,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
} from "@ryuzi/ui";
import { commands, type ToolPolicyRow } from "@/bindings";
import { useStore } from "@/store";
import { LOCAL_RUNNER } from "@/lib/session-key";

// Settings → Permissions: lists every persisted "don't ask again" rule
// (allowAlways/rejectAlways) created from approval prompts, and lets the user
// revoke one. Rules live in the `tool_policies` table; there is no create UI
// here by design — rules are only ever created from an approval prompt.
export function PermissionsCard() {
  const projects = useStore((s) => s.projects);
  const [rules, setRules] = useState<ToolPolicyRow[]>([]);

  const load = useCallback(async () => {
    const res = await commands.listToolPolicies(LOCAL_RUNNER);
    if (res.status === "ok") setRules(res.data);
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  const projectName = (id: string) => projects.find((p) => p.projectId === id)?.name ?? id;

  const revoke = async (row: ToolPolicyRow) => {
    await commands.deleteToolPolicy(LOCAL_RUNNER, row.projectId, row.tool);
    void load();
  };

  return (
    <Card className="mt-3">
      <CardHeader>
        <div className="min-w-0">
          <CardTitle>Permissions</CardTitle>
          <CardHint>Always-allow / always-deny rules created from approval prompts.</CardHint>
        </div>
      </CardHeader>

      {rules.length === 0 ? (
        <div className="px-[18px] py-3 text-[13px] text-muted-foreground">No saved rules.</div>
      ) : (
        rules.map((r) => (
          <CardRow key={`${r.projectId}:${r.tool}`}>
            <div className="min-w-0 flex-1">
              <div className="truncate text-[13px] font-medium">{r.tool}</div>
              <div className="truncate text-[12px] text-muted-foreground">{projectName(r.projectId)}</div>
            </div>
            <Badge variant={r.decision === "allowAlways" ? "secondary" : "destructive"}>
              {r.decision === "allowAlways" ? "Always allow" : "Always deny"}
            </Badge>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={`Remove rule for ${r.tool}`}
              title={`Remove rule for ${r.tool}`}
              onClick={() => void revoke(r)}
            >
              <Trash2 aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
            </Button>
          </CardRow>
        ))
      )}
    </Card>
  );
}
