import { useMemo, useState } from "react";
import { AlertTriangle, ChevronRight, Plus } from "lucide-react";
import { Badge, Button, Combobox, Segmented, SettingsCard } from "@ryuzi/ui";
import type { AgentModelInfo, AgentSummaryInfo } from "@/bindings";
import { ModelPicker } from "@/components/ModelPicker";
import { AgentActionsMenu } from "@/components/agents/AgentActionsMenu";
import { AgentEditorModal } from "@/components/agents/AgentEditorModal";
import { useAgents } from "@/store-agents";
import { useNav } from "@/store-nav";

const AVATAR_COLORS: Record<string, string> = {
  violet: "#8B5CF6",
  blue: "#3B82F6",
  cyan: "#06B6D4",
  emerald: "#10B981",
  amber: "#F59E0B",
  rose: "#F43F5E",
};

function modelValue(model: AgentModelInfo): string {
  return model.kind === "route" ? model.route : model.name;
}

function modelLabel(model: AgentModelInfo): string {
  return modelValue(model);
}

function permissionLabel(mode: string): string {
  switch (mode) {
    case "accept_edits":
      return "Edit";
    case "full":
      return "Full";
    case "plan":
      return "Plan";
    default:
      return "Ask";
  }
}

function AgentRow({ agent }: { agent: AgentSummaryInfo }) {
  const nav = useNav();
  return (
    <SettingsCard className="flex h-[92px] items-stretch">
      <Button
        type="button"
        variant="ghost"
        aria-label={`Open ${agent.name}`}
        onClick={() => nav.navigate({ kind: "agentDetail", agentId: agent.id })}
        className="h-full min-w-0 flex-1 justify-start gap-3 rounded-none px-[18px] text-left font-normal hover:bg-accent/50"
      >
        <span
          aria-hidden
          className="size-9 shrink-0 rounded-lg border border-white/10"
          style={{ backgroundColor: AVATAR_COLORS[agent.avatarColor] ?? agent.avatarColor }}
        />
        <span className="min-w-0 flex-1">
          <span className="flex items-center gap-2">
            <span className="truncate text-[13.5px] font-semibold text-foreground">{agent.name}</span>
            {agent.isDefault && <Badge variant="secondary">Default</Badge>}
            {!agent.executable && (
              <Badge variant="destructive">
                <AlertTriangle aria-hidden size={11} strokeWidth={2} /> Invalid
              </Badge>
            )}
          </span>
          <span className="mt-1 block truncate text-xs text-muted-foreground">{agent.description}</span>
          <span className="mt-1.5 flex items-center gap-2.5 text-[11px] text-muted-foreground">
            <span className="font-mono text-foreground">{modelLabel(agent.model)}</span>
            <Badge variant="outline" className="h-[18px] px-1.5 text-[10px]">
              {permissionLabel(agent.permissionMode)}
            </Badge>
            <span>
              {agent.skillCount} {agent.skillCount === 1 ? "skill" : "skills"} · {agent.toolCount}{" "}
              {agent.toolCount === 1 ? "tool" : "tools"}
            </span>
          </span>
        </span>
        <ChevronRight aria-hidden size={14} strokeWidth={2} className="shrink-0 text-muted-foreground" />
      </Button>
      <span className="flex w-12 shrink-0 items-center justify-center border-l border-border">
        <AgentActionsMenu agent={agent} />
      </span>
    </SettingsCard>
  );
}

function SubagentSettings() {
  const registry = useAgents((s) => s.registry);
  const models = useAgents((s) => s.models);
  const saving = useAgents((s) => s.saving);
  const selected = registry?.subagentModel;
  const selectedValue = selected ? modelValue(selected) : "";
  const selectedInfo = models.find((model) => model.requestValue === selectedValue) ?? null;
  const effortOptions = selected?.kind === "concrete" ? (selectedInfo?.supported ?? []) : [];

  const updateModel = (value: string) => {
    const info = models.find((model) => model.requestValue === value);
    const next: AgentModelInfo =
      info?.kind === "namedRoute"
        ? { kind: "route", route: value }
        : {
            kind: "concrete",
            name: value,
            effort:
              selected?.kind === "concrete" && info?.supported.some((option) => option.value === selected.effort) ? selected.effort : null,
          };
    void useAgents.getState().updateSubagentModel(next);
  };

  return (
    <div className="flex flex-col gap-3">
      <p className="m-0 text-[13px] leading-5 text-muted-foreground">
        Subagents are ephemeral, memoryless runtime workers. They share one model configuration and are created automatically for delegated
        work.
      </p>
      <SettingsCard>
        <div className="flex items-center gap-3 px-[18px] py-3.5">
          <span className="w-[168px] shrink-0">
            <span className="block text-[13px] font-medium">Shared model</span>
            <span className="block text-[11px] text-muted-foreground">Used by every subagent</span>
          </span>
          <ModelPicker
            ariaLabel="Shared subagent model"
            variant="field"
            models={models.map((model) => model.requestValue)}
            value={selectedValue}
            onValueChange={updateModel}
            disabled={saving || models.length === 0}
          />
          {effortOptions.length > 0 && selected?.kind === "concrete" && (
            <Combobox
              aria-label="Shared subagent effort"
              className="w-[170px]"
              options={[
                { value: "", label: "Model default" },
                ...effortOptions.map((option) => ({
                  value: option.value,
                  label: option.label,
                  description: option.description ?? undefined,
                })),
              ]}
              value={selected.effort ?? ""}
              onValueChange={(effort) =>
                void useAgents.getState().updateSubagentModel({ ...selected, effort: effort === "" ? null : effort })
              }
              disabled={saving}
            />
          )}
        </div>
      </SettingsCard>
      <p className="m-0 text-xs leading-5 text-muted-foreground">
        Main agents own durable identity and knowledge. Subagents do not have profiles to create or edit.
      </p>
    </div>
  );
}

export function AgentsView() {
  const [tab, setTab] = useState<"main" | "sub">("main");
  const [createOpen, setCreateOpen] = useState(false);
  const registry = useAgents((s) => s.registry);
  const loading = useAgents((s) => s.loading);
  const agents = useMemo(() => registry?.agents ?? [], [registry]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <div className="mb-5 flex min-h-10 items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Agents</h2>
            <p className="m-0 text-[13px] text-muted-foreground">Manage durable main agents and shared subagent runtime defaults.</p>
          </div>
          {tab === "main" && (
            <Button onClick={() => setCreateOpen(true)} aria-label="New agent" className="shrink-0">
              <Plus aria-hidden size={14} strokeWidth={2} /> New agent
            </Button>
          )}
        </div>
        <div className="mb-4">
          <Segmented
            options={[
              { id: "main", label: "Main Agent" },
              { id: "sub", label: "Sub Agent" },
            ]}
            value={tab}
            onChange={setTab}
          />
        </div>
        {tab === "main" ? (
          <div className="flex flex-col gap-2.5">
            {agents.map((agent) => (
              <AgentRow key={agent.id} agent={agent} />
            ))}
            {!loading && agents.length === 0 && <p className="py-8 text-center text-[13px] text-muted-foreground">No agents found.</p>}
          </div>
        ) : (
          <SubagentSettings />
        )}
      </div>
      <AgentEditorModal open={createOpen} onClose={() => setCreateOpen(false)} />
    </div>
  );
}
