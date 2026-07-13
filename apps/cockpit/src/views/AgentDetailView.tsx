import { useEffect, useState } from "react";
import { AlertTriangle, ArrowLeft } from "lucide-react";
import { Badge, Button, Segmented, SettingsCard, SettingsCardTitle } from "@ryuzi/ui";
import { AgentActionsMenu } from "@/components/agents/AgentActionsMenu";
import { AgentAdvancedTab } from "@/components/agents/AgentAdvancedTab";
import { AgentModelTab } from "@/components/agents/AgentModelTab";
import { AgentPermissionsTab } from "@/components/agents/AgentPermissionsTab";
import { AgentSkillsToolsTab } from "@/components/agents/AgentSkillsToolsTab";
import { useAgents } from "@/store-agents";
import { useNav } from "@/store-nav";

const TABS = [
  { id: "overview", label: "Overview" },
  { id: "model", label: "Model" },
  { id: "permissions", label: "Permissions" },
  { id: "capabilities", label: "Skills & Tools" },
  { id: "learning", label: "Learning" },
  { id: "advanced", label: "Advanced" },
] as const;
type Tab = (typeof TABS)[number]["id"];
const COLORS: Record<string, string> = {
  violet: "#8B5CF6",
  blue: "#3B82F6",
  cyan: "#06B6D4",
  emerald: "#10B981",
  amber: "#F59E0B",
  rose: "#F43F5E",
};

function metric(value: number, singular: string, plural: string) {
  return `${value} ${value === 1 ? singular : plural}`;
}

export function AgentDetailView({ agentId }: { agentId: string }) {
  const detail = useAgents((state) => (state.detail?.summary.id === agentId ? state.detail : null));
  const loading = useAgents((state) => state.loading);
  const [tab, setTab] = useState<Tab>("overview");
  const nav = useNav();
  useEffect(() => {
    if (!detail) void useAgents.getState().loadDetail(agentId);
  }, [agentId, detail]);

  if (!detail)
    return (
      <div className="flex flex-1 items-center justify-center text-[13px] text-muted-foreground">
        {loading ? "Loading agent…" : "Agent not found."}
      </div>
    );
  const { summary } = detail;
  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-5">
      <div className="mx-auto max-w-[920px]">
        <header className="flex h-[52px] items-center gap-3 border-b border-border">
          <Button variant="ghost" size="sm" aria-label="Back to Agents" onClick={nav.goBack} className="-ml-2 shrink-0">
            <ArrowLeft aria-hidden size={14} /> Agents
          </Button>
          <span
            aria-hidden
            className="size-8 shrink-0 rounded-lg border border-white/10"
            style={{ backgroundColor: COLORS[summary.avatarColor] ?? summary.avatarColor }}
          />
          <div className="min-w-0 flex-1">
            <h2 className="m-0 truncate text-lg font-semibold">{summary.name}</h2>
            <p className="m-0 truncate text-[11px] text-muted-foreground">{summary.description}</p>
          </div>
          {summary.isDefault ? <Badge variant="secondary">Default</Badge> : null}
          <Badge variant={summary.executable ? "outline" : "destructive"}>
            {summary.executable ? (
              "Executable"
            ) : (
              <>
                <AlertTriangle aria-hidden size={11} /> Invalid
              </>
            )}
          </Badge>
          <AgentActionsMenu agent={summary} />
        </header>
        <div className="my-4 overflow-x-auto">
          <Segmented options={[...TABS]} value={tab} onChange={setTab} />
        </div>
        {summary.validation.length > 0 ? (
          <SettingsCard className="mb-3 border-destructive/40 px-[18px] py-3">
            <SettingsCardTitle>Configuration issues</SettingsCardTitle>
            <ul className="mb-0 mt-2 pl-4 text-xs text-destructive">
              {summary.validation.map((issue) => (
                <li key={`${issue.field}:${issue.message}`}>
                  <strong>{issue.field}:</strong> {issue.message}
                </li>
              ))}
            </ul>
          </SettingsCard>
        ) : null}
        {tab === "overview" ? (
          <div className="grid grid-cols-3 gap-3">
            <SettingsCard className="px-[18px] py-4">
              <span className="block text-[11px] text-muted-foreground">Knowledge</span>
              <strong className="mt-1 block text-[13px]">{metric(summary.knowledgeCount, "readable concept", "readable concepts")}</strong>
            </SettingsCard>
            <SettingsCard className="px-[18px] py-4">
              <span className="block text-[11px] text-muted-foreground">Skills</span>
              <strong className="mt-1 block text-[13px]">{metric(summary.skillCount, "enabled skill", "enabled skills")}</strong>
            </SettingsCard>
            <SettingsCard className="px-[18px] py-4">
              <span className="block text-[11px] text-muted-foreground">Tools</span>
              <strong className="mt-1 block text-[13px]">{metric(summary.toolCount, "enabled tool", "enabled tools")}</strong>
            </SettingsCard>
            <SettingsCard className="col-span-3 px-[18px] py-4">
              <SettingsCardTitle>Recent sessions</SettingsCardTitle>
              <p className="mb-0 mt-3 text-xs text-muted-foreground">No owned sessions yet.</p>
            </SettingsCard>
          </div>
        ) : null}
        {tab === "model" ? <AgentModelTab detail={detail} /> : null}
        {tab === "permissions" ? <AgentPermissionsTab detail={detail} /> : null}
        {tab === "capabilities" ? <AgentSkillsToolsTab detail={detail} /> : null}
        {tab === "learning" ? (
          <SettingsCard className="px-[18px] py-5">
            <SettingsCardTitle>Learning</SettingsCardTitle>
            <p className="mb-0 mt-2 text-xs text-muted-foreground">Per-agent learning controls are coming in Task 9.</p>
          </SettingsCard>
        ) : null}
        {tab === "advanced" ? <AgentAdvancedTab detail={detail} /> : null}
      </div>
    </div>
  );
}
