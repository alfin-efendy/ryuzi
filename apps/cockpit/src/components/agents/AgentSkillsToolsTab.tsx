import { useEffect, useState } from "react";
import { Button, Input, SettingsCard, SettingsCardRow, SettingsCardTitle } from "@ryuzi/ui";
import { X } from "lucide-react";
import type { AgentDetailInfo } from "@/bindings";
import { useAgents } from "@/store-agents";
import { mutationFromDetail } from "./agentMutation";

type ListKey = "skills" | "nativeTools" | "pluginTools" | "apps";
const GROUPS: { key: ListKey; title: string; label: string; add: string }[] = [
  { key: "skills", title: "Skills", label: "Skill ID", add: "Add skill" },
  { key: "nativeTools", title: "Native tools", label: "Native tool ID", add: "Add native tool" },
  { key: "pluginTools", title: "Plugin tools", label: "Plugin tool ID", add: "Add plugin tool" },
  { key: "apps", title: "Apps & MCP", label: "App ID", add: "Add app" },
];

export function AgentSkillsToolsTab({ detail }: { detail: AgentDetailInfo }) {
  const saving = useAgents((state) => state.saving);
  const [lists, setLists] = useState<Record<ListKey, string[]>>({
    skills: detail.skills,
    nativeTools: detail.nativeTools,
    pluginTools: detail.pluginTools,
    apps: detail.apps,
  });
  const [drafts, setDrafts] = useState<Record<ListKey, string>>({ skills: "", nativeTools: "", pluginTools: "", apps: "" });
  useEffect(
    () => setLists({ skills: detail.skills, nativeTools: detail.nativeTools, pluginTools: detail.pluginTools, apps: detail.apps }),
    [detail],
  );

  const add = (key: ListKey) => {
    const id = drafts[key].trim();
    if (!id || lists[key].includes(id)) return;
    setLists((current) => ({ ...current, [key]: [...current[key], id] }));
    setDrafts((current) => ({ ...current, [key]: "" }));
  };

  return (
    <div className="flex flex-col gap-3">
      {GROUPS.map((group) => (
        <SettingsCard key={group.key}>
          <div className="border-b border-border px-[18px] py-3">
            <SettingsCardTitle>{group.title}</SettingsCardTitle>
          </div>
          {lists[group.key].map((id) => (
            <SettingsCardRow key={id} className="gap-2">
              <code className="min-w-0 flex-1 truncate text-xs">{id}</code>
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={`Remove ${id}`}
                onClick={() => setLists((current) => ({ ...current, [group.key]: current[group.key].filter((item) => item !== id) }))}
              >
                <X aria-hidden size={13} />
              </Button>
            </SettingsCardRow>
          ))}
          <SettingsCardRow className="gap-2">
            <Input
              aria-label={group.label}
              value={drafts[group.key]}
              onChange={(event) => setDrafts((current) => ({ ...current, [group.key]: event.target.value }))}
              placeholder="Stable ID"
            />
            <Button variant="outline" onClick={() => add(group.key)}>
              {group.add}
            </Button>
          </SettingsCardRow>
        </SettingsCard>
      ))}
      <div className="flex justify-end">
        <Button
          disabled={saving}
          onClick={() => void useAgents.getState().update(detail.summary.id, { ...mutationFromDetail(detail), ...lists })}
        >
          Save skills and tools
        </Button>
      </div>
    </div>
  );
}
