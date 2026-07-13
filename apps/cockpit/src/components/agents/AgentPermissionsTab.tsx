import { useEffect, useState } from "react";
import { Button, Combobox, Input, SettingsCard, SettingsCardRow, SettingsCardTitle } from "@ryuzi/ui";
import { Trash2 } from "lucide-react";
import type { AgentDetailInfo, PermissionRuleInfo } from "@/bindings";
import { useAgents } from "@/store-agents";
import { mutationFromDetail } from "./agentMutation";

const MODES = [
  { value: "ask", label: "Ask" },
  { value: "accept_edits", label: "Accept edits" },
  { value: "full", label: "Full access" },
  { value: "plan", label: "Plan only" },
];

export function AgentPermissionsTab({ detail }: { detail: AgentDetailInfo }) {
  const saving = useAgents((state) => state.saving);
  const [mode, setMode] = useState(detail.summary.permissionMode);
  const [rules, setRules] = useState<PermissionRuleInfo[]>(detail.permissionRules);

  useEffect(() => {
    setMode(detail.summary.permissionMode);
    setRules(detail.permissionRules);
  }, [detail]);

  const patch = (id: string, values: Partial<PermissionRuleInfo>) =>
    setRules((current) => current.map((rule) => (rule.id === id ? { ...rule, ...values } : rule)));
  const add = () => setRules((current) => [...current, { id: crypto.randomUUID(), tool: "", decision: "allow", commandPrefix: null }]);
  const hasInvalidRule = rules.some((rule) => rule.tool.trim() === "");

  return (
    <div className="flex flex-col gap-3">
      <SettingsCard>
        <div className="border-b border-border px-[18px] py-3.5">
          <SettingsCardTitle>Permission mode</SettingsCardTitle>
        </div>
        <SettingsCardRow className="gap-4">
          <span className="min-w-0 flex-1">
            <span className="block text-[13px] font-medium">Default behavior</span>
            <span className="block text-[11px] text-muted-foreground">Applied when no explicit rule matches.</span>
          </span>
          <Combobox
            aria-label="Permission mode"
            className="w-[190px]"
            options={MODES}
            value={mode}
            onValueChange={setMode}
            disabled={saving}
          />
        </SettingsCardRow>
      </SettingsCard>
      <SettingsCard>
        <div className="flex items-center border-b border-border px-[18px] py-3">
          <span className="flex-1">
            <SettingsCardTitle>Explicit rules</SettingsCardTitle>
          </span>
          <Button variant="outline" size="sm" onClick={add}>
            Add rule
          </Button>
        </div>
        {rules.length === 0 ? (
          <p className="m-0 px-[18px] py-5 text-xs text-muted-foreground">No explicit rules.</p>
        ) : (
          rules.map((rule) => (
            <SettingsCardRow key={rule.id} className="gap-2">
              <Input
                aria-label="Rule tool ID"
                className="w-[190px]"
                placeholder="Stable tool ID"
                value={rule.tool}
                onChange={(event) => patch(rule.id, { tool: event.target.value })}
              />
              <Combobox
                aria-label="Rule decision"
                className="w-[130px]"
                options={[
                  { value: "allow", label: "Allow" },
                  { value: "deny", label: "Deny" },
                ]}
                value={rule.decision}
                onValueChange={(decision) => patch(rule.id, { decision })}
              />
              <Input
                aria-label="Command prefix"
                className="min-w-0 flex-1"
                placeholder="Optional command prefix"
                value={rule.commandPrefix ?? ""}
                onChange={(event) => patch(rule.id, { commandPrefix: event.target.value })}
              />
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={`Remove rule ${rule.tool}`}
                onClick={() => setRules((current) => current.filter((item) => item.id !== rule.id))}
              >
                <Trash2 aria-hidden size={14} />
              </Button>
            </SettingsCardRow>
          ))
        )}
        <div className="flex justify-end border-t border-border px-[18px] py-3">
          <Button
            disabled={saving || hasInvalidRule}
            onClick={() =>
              void useAgents.getState().update(detail.summary.id, {
                ...mutationFromDetail(detail),
                permissionMode: mode,
                permissionRules: rules.map((rule) => ({
                  ...rule,
                  tool: rule.tool.trim(),
                  commandPrefix: rule.commandPrefix?.trim() || null,
                })),
              })
            }
          >
            Save permissions
          </Button>
        </div>
      </SettingsCard>
    </div>
  );
}
