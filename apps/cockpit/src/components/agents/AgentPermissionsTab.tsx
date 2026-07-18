import { useEffect, useMemo, useState } from "react";
import { Button, Combobox, Input, SettingsCard, SettingsCardRow, SettingsCardTitle } from "@ryuzi/ui";
import { Trash2 } from "lucide-react";
import type { AgentDetailInfo, PermissionRuleInfo } from "@/bindings";
import { useAgents } from "@/store-agents";
import { useAgentConfigurationCatalog } from "@/store-agent-catalog";
import { mutationFromDetail } from "./agentMutation";

const MODES = [
  { value: "ask", label: "Ask" },
  { value: "accept_edits", label: "Accept edits" },
  { value: "full", label: "Full access" },
  { value: "plan", label: "Plan only" },
];

const DECISIONS = [
  { value: "allow", label: "Allow" },
  { value: "ask", label: "Ask" },
  { value: "deny", label: "Deny" },
];

function ruleKey(rule: PermissionRuleInfo): string {
  return `${rule.tool.trim()}\u0000${rule.commandPrefix?.trim() || ""}`;
}

export function AgentPermissionsTab({ detail }: { detail: AgentDetailInfo }) {
  const saving = useAgents((state) => state.saving);
  const catalog = useAgentConfigurationCatalog((state) => state.catalog);
  const loadCatalog = useAgentConfigurationCatalog((state) => state.load);
  const [mode, setMode] = useState(detail.summary.permissionMode);
  const [rules, setRules] = useState<PermissionRuleInfo[]>(detail.permissionRules);

  useEffect(() => {
    void loadCatalog();
  }, [loadCatalog]);

  useEffect(() => {
    setMode(detail.summary.permissionMode);
    setRules(detail.permissionRules);
  }, [detail]);

  const toolCatalog = useMemo(() => {
    const entries = [...(catalog?.nativeTools ?? []), ...(catalog?.pluginTools ?? [])].filter((entry) => entry.available);
    return Array.from(new Map(entries.map((entry) => [entry.id, entry])).values());
  }, [catalog]);
  const entryById = useMemo(() => new Map(toolCatalog.map((entry) => [entry.id, entry])), [toolCatalog]);
  const duplicateKeys = useMemo(() => {
    const counts = new Map<string, number>();
    for (const rule of rules) {
      if (rule.tool.trim()) counts.set(ruleKey(rule), (counts.get(ruleKey(rule)) ?? 0) + 1);
    }
    return new Set([...counts.entries()].filter(([, count]) => count > 1).map(([key]) => key));
  }, [rules]);

  const patch = (id: string, values: Partial<PermissionRuleInfo>) =>
    setRules((current) => current.map((rule) => (rule.id === id ? { ...rule, ...values } : rule)));
  const add = () => setRules((current) => [...current, { id: crypto.randomUUID(), tool: "", decision: "allow", commandPrefix: null }]);

  const hasInvalidRule = rules.some((rule) => {
    const entry = entryById.get(rule.tool);
    return rule.tool.trim() === "" || catalog === null || entry === undefined || duplicateKeys.has(ruleKey(rule));
  });

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
          <Button variant="outline" size="sm" onClick={add} disabled={saving}>
            Add rule
          </Button>
        </div>
        {rules.length === 0 ? (
          <p className="m-0 px-[18px] py-5 text-xs text-muted-foreground">No explicit rules.</p>
        ) : (
          rules.map((rule) => {
            const entry = entryById.get(rule.tool);
            const unavailable = rule.tool.trim() !== "" && catalog !== null && (entry === undefined || !entry.available);
            const duplicate = duplicateKeys.has(ruleKey(rule));
            const options = [
              ...toolCatalog.map((tool) => ({
                value: tool.id,
                label: tool.label,
                description: tool.description || tool.id,
                mono: true,
              })),
              ...(unavailable ? [{ value: rule.tool, label: "Unavailable", description: rule.tool, mono: true, invalid: true }] : []),
            ];

            return (
              <SettingsCardRow key={rule.id} className="flex-wrap gap-2">
                <div className="min-w-0 w-[190px]">
                  <Combobox
                    aria-label="Rule tool"
                    className="w-full"
                    placeholder="Select a tool…"
                    options={options}
                    searchThreshold={0}
                    value={rule.tool || null}
                    onValueChange={(tool) =>
                      patch(rule.id, {
                        tool,
                        commandPrefix: entryById.get(tool)?.commandScoped ? rule.commandPrefix : null,
                      })
                    }
                    disabled={saving || catalog === null}
                  />
                  {unavailable ? <code className="mt-1 block truncate text-[11px] text-destructive">{rule.tool}</code> : null}
                  {duplicate ? <span className="mt-1 block text-[11px] text-destructive">Duplicate permission rule.</span> : null}
                </div>
                <Combobox
                  aria-label="Rule decision"
                  className="w-[130px]"
                  options={DECISIONS}
                  value={rule.decision}
                  onValueChange={(decision) => patch(rule.id, { decision })}
                  disabled={saving}
                />
                {entry?.commandScoped ? (
                  <Input
                    aria-label="Command prefix"
                    className="min-w-0 flex-1"
                    placeholder="Optional command prefix"
                    value={rule.commandPrefix ?? ""}
                    onChange={(event) => patch(rule.id, { commandPrefix: event.target.value })}
                    disabled={saving}
                  />
                ) : null}
                <Button
                  variant="ghost"
                  size="icon-sm"
                  aria-label={`Remove${unavailable ? " unavailable" : ""} rule ${rule.tool}`}
                  onClick={() => setRules((current) => current.filter((item) => item.id !== rule.id))}
                  disabled={saving}
                >
                  <Trash2 aria-hidden size={14} />
                </Button>
              </SettingsCardRow>
            );
          })
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
