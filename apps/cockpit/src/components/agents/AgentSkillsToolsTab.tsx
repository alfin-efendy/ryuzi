import { useEffect, useState } from "react";
import { Trash2 } from "lucide-react";
import { Button, Combobox, Input, SettingsCard, SettingsCardRow, SettingsCardTitle, Switch } from "@ryuzi/ui";
import type { AgentDetailInfo } from "@/bindings";
import { useAgents } from "@/store-agents";
import { useApps } from "@/store-apps";
import { usePlugins } from "@/store-plugins";
import { mutationFromDetail } from "./agentMutation";

type StableIdGroupProps = {
  title: string;
  singular: string;
  inputLabel: string;
  values: string[];
  input: string;
  saving: boolean;
  onInput: (value: string) => void;
  onChange: (values: string[]) => void;
};

function StableIdGroup({ title, singular, inputLabel, values, input, saving, onInput, onChange }: StableIdGroupProps) {
  const candidate = input.trim();
  const addDisabled = saving || candidate === "" || values.includes(candidate);
  return (
    <SettingsCard>
      <div className="border-b border-border px-[18px] py-3.5">
        <SettingsCardTitle>{title}</SettingsCardTitle>
      </div>
      <SettingsCardRow className="gap-2">
        <Input
          aria-label={inputLabel}
          className="min-w-0 flex-1"
          placeholder={`Stable ${singular} ID`}
          value={input}
          disabled={saving}
          onChange={(event) => onInput(event.target.value)}
        />
        <Button
          variant="outline"
          disabled={addDisabled}
          onClick={() => {
            onChange([...values, candidate]);
            onInput("");
          }}
        >
          Add {singular}
        </Button>
      </SettingsCardRow>
      {values.length === 0 ? (
        <p className="m-0 px-[18px] py-4 text-xs text-muted-foreground">No {title.toLowerCase()} enabled.</p>
      ) : (
        values.map((id) => (
          <SettingsCardRow key={id} className="gap-3">
            <code className="min-w-0 flex-1 truncate text-xs">{id}</code>
            <Button
              variant="ghost"
              size="icon-sm"
              aria-label={`Remove ${singular} ${id}`}
              disabled={saving}
              onClick={() => onChange(values.filter((value) => value !== id))}
            >
              <Trash2 aria-hidden size={14} />
            </Button>
          </SettingsCardRow>
        ))
      )}
    </SettingsCard>
  );
}

export function AgentSkillsToolsTab({ detail }: { detail: AgentDetailInfo }) {
  const saving = useAgents((state) => state.saving);
  const catalogApps = useApps((state) => state.apps);
  const plugins = usePlugins((state) => state.plugins);
  const [skills, setSkills] = useState(detail.skills);
  const [nativeTools, setNativeTools] = useState(detail.nativeTools);
  const [pluginTools, setPluginTools] = useState(detail.pluginTools);
  const [apps, setApps] = useState(detail.apps);
  const [skillInput, setSkillInput] = useState("");
  const [nativeInput, setNativeInput] = useState("");
  const [pluginInput, setPluginInput] = useState("");

  useEffect(() => {
    setSkills(detail.skills);
    setNativeTools(detail.nativeTools);
    setPluginTools(detail.pluginTools);
    setApps(detail.apps);
  }, [detail]);

  const appById = new Map(catalogApps.map((app) => [app.id, app]));
  const availableToAdd = catalogApps.filter((app) => !apps.includes(app.id));
  const appRows = [
    ...apps.map((id) => ({ id, app: appById.get(id) })),
    ...catalogApps.filter((app) => !apps.includes(app.id)).map((app) => ({ id: app.id, app })),
  ];

  return (
    <div className="flex flex-col gap-3">
      <StableIdGroup
        title="Skills"
        singular="skill"
        inputLabel="Skill ID"
        values={skills}
        input={skillInput}
        saving={saving}
        onInput={setSkillInput}
        onChange={setSkills}
      />
      <StableIdGroup
        title="Native tools"
        singular="native tool"
        inputLabel="Native tool ID"
        values={nativeTools}
        input={nativeInput}
        saving={saving}
        onInput={setNativeInput}
        onChange={setNativeTools}
      />
      <StableIdGroup
        title="Plugin tools"
        singular="plugin tool"
        inputLabel="Plugin tool ID"
        values={pluginTools}
        input={pluginInput}
        saving={saving}
        onInput={setPluginInput}
        onChange={setPluginTools}
      />
      {plugins.length > 0 ? (
        <p className="-mt-2 mb-0 px-1 text-[11px] text-muted-foreground">
          {plugins.length} {plugins.length === 1 ? "plugin is" : "plugins are"} installed; enter tool IDs exposed by their manifests.
        </p>
      ) : null}
      <SettingsCard>
        <div className="flex items-center gap-3 border-b border-border px-[18px] py-3.5">
          <span className="min-w-0 flex-1">
            <SettingsCardTitle>Apps &amp; MCP</SettingsCardTitle>
          </span>
          <Combobox
            aria-label="App catalog"
            className="w-[220px]"
            placeholder="Enable an app…"
            options={availableToAdd.map((app) => ({ value: app.id, label: app.name, description: app.id }))}
            value={null}
            disabled={saving || availableToAdd.length === 0}
            onValueChange={(id) => setApps((current) => (current.includes(id) ? current : [...current, id]))}
          />
        </div>
        <div data-testid="agent-app-rows">
          {appRows.length === 0 ? (
            <p className="m-0 px-[18px] py-4 text-xs text-muted-foreground">No apps available.</p>
          ) : (
            appRows.map(({ id, app }) => {
              const enabled = apps.includes(id);
              return (
                <SettingsCardRow key={id} className="gap-3">
                  <span className="min-w-0 flex-1">
                    <span className="block text-[13px] font-medium">{app?.name ?? id}</span>
                    <span className="block text-[11px] text-muted-foreground">{app ? app.id : "Unavailable"}</span>
                  </span>
                  {app ? (
                    <Switch
                      on={enabled}
                      label={`Enable app ${id}`}
                      onToggle={() =>
                        setApps((current) =>
                          enabled ? current.filter((value) => value !== id) : current.includes(id) ? current : [...current, id],
                        )
                      }
                    />
                  ) : (
                    <Button
                      variant="ghost"
                      size="icon-sm"
                      aria-label={`Remove unavailable app ${id}`}
                      onClick={() => setApps((current) => current.filter((value) => value !== id))}
                    >
                      <Trash2 aria-hidden size={14} />
                    </Button>
                  )}
                </SettingsCardRow>
              );
            })
          )}
        </div>
      </SettingsCard>
      <div className="flex justify-end">
        <Button
          disabled={saving}
          onClick={() =>
            void useAgents.getState().update(detail.summary.id, {
              ...mutationFromDetail(detail),
              skills,
              nativeTools,
              pluginTools,
              apps,
            })
          }
        >
          Save skills and tools
        </Button>
      </div>
    </div>
  );
}
