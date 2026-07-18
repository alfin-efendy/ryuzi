import { useEffect, useState } from "react";
import { Trash2 } from "lucide-react";
import { Button, Combobox, SettingsCard, SettingsCardRow, SettingsCardTitle, Switch } from "@ryuzi/ui";
import type { AgentDetailInfo, CatalogEntryInfo } from "@/bindings";
import { useAgents } from "@/store-agents";
import { useAgentConfigurationCatalog } from "@/store-agent-catalog";
import { mutationFromDetail } from "./agentMutation";

type CatalogGroupProps = {
  title: string;
  singular: string;
  values: string[];
  catalog: CatalogEntryInfo[] | undefined;
  saving: boolean;
  onChange: (values: string[]) => void;
};

function CatalogIdGroup({ title, singular, values, catalog, saving, onChange }: CatalogGroupProps) {
  const availableToAdd = (catalog ?? []).filter((entry) => entry.available && !values.includes(entry.id));
  const entryById = new Map((catalog ?? []).map((entry) => [entry.id, entry]));

  return (
    <SettingsCard>
      <div className="flex items-center gap-3 border-b border-border px-[18px] py-3.5">
        <SettingsCardTitle>{title}</SettingsCardTitle>
        <Combobox
          aria-label={`${singular.charAt(0).toUpperCase()}${singular.slice(1)} catalog`}
          className="ml-auto w-[220px]"
          placeholder={`Enable a ${singular}…`}
          options={availableToAdd.map((entry) => ({
            value: entry.id,
            label: entry.label,
            description: entry.description || entry.id,
          }))}
          value={null}
          disabled={saving || catalog === undefined || availableToAdd.length === 0}
          onValueChange={(id) => onChange(values.includes(id) ? values : [...values, id])}
        />
      </div>
      {values.length === 0 ? (
        <p className="m-0 px-[18px] py-4 text-xs text-muted-foreground">No {title.toLowerCase()} enabled.</p>
      ) : (
        values.map((id) => {
          const entry = entryById.get(id);
          const available = entry?.available === true;
          return (
            <SettingsCardRow key={id} className="gap-3">
              <span className="min-w-0 flex-1">
                <span className={`block text-[13px] font-medium${available ? "" : " text-destructive"}`}>
                  {available ? entry.label : "Unavailable"}
                </span>
                <code className="block truncate text-[11px] text-muted-foreground">{id}</code>
              </span>
              <Button
                variant="ghost"
                size="icon-sm"
                aria-label={`Remove${available ? "" : " unavailable"} ${singular} ${id}`}
                disabled={saving}
                onClick={() => onChange(values.filter((value) => value !== id))}
              >
                <Trash2 aria-hidden size={14} />
              </Button>
            </SettingsCardRow>
          );
        })
      )}
    </SettingsCard>
  );
}

export function AgentSkillsToolsTab({ detail }: { detail: AgentDetailInfo }) {
  const saving = useAgents((state) => state.saving);
  const catalog = useAgentConfigurationCatalog((state) => state.catalog);
  const catalogLoading = useAgentConfigurationCatalog((state) => state.loading);
  const catalogError = useAgentConfigurationCatalog((state) => state.error);
  const loadCatalog = useAgentConfigurationCatalog((state) => state.load);
  const [skills, setSkills] = useState(detail.skills);
  const [nativeTools, setNativeTools] = useState(detail.nativeTools);
  const [pluginTools, setPluginTools] = useState(detail.pluginTools);
  const [apps, setApps] = useState(detail.apps);

  useEffect(() => {
    void loadCatalog();
  }, [loadCatalog]);

  useEffect(() => {
    setSkills(detail.skills);
    setNativeTools(detail.nativeTools);
    setPluginTools(detail.pluginTools);
    setApps(detail.apps);
  }, [detail]);

  const catalogApps = catalog?.apps ?? [];
  const appById = new Map(catalogApps.map((app) => [app.id, app]));
  const availableToAdd = catalogApps.filter((app) => app.available && !apps.includes(app.id));
  const appRows = [
    ...apps.map((id) => ({ id, app: appById.get(id) })),
    ...catalogApps.filter((app) => !apps.includes(app.id)).map((app) => ({ id: app.id, app })),
  ];
  const hasUnavailable =
    catalog !== null &&
    [
      [skills, catalog.skills],
      [nativeTools, catalog.nativeTools],
      [pluginTools, catalog.pluginTools],
      [apps, catalog.apps],
    ].some(([values, entries]) =>
      (values as string[]).some((id) => !(entries as CatalogEntryInfo[]).some((entry) => entry.id === id && entry.available)),
    );

  if (catalogLoading || catalogError || catalog === null) {
    return (
      <SettingsCard>
        <div className="px-[18px] py-4 text-xs text-muted-foreground" role={catalogError ? "alert" : undefined}>
          {catalogLoading ? "Loading skills and tools…" : catalogError ? `Couldn't load skills and tools: ${catalogError}` : "Loading skills and tools…"}
        </div>
        <div className="flex justify-end border-t border-border px-[18px] py-3">
          <Button disabled>
            Save skills and tools
          </Button>
        </div>
      </SettingsCard>
    );
  }

  return (
    <div className="flex flex-col gap-3">
      <CatalogIdGroup
        title="Skills"
        singular="skill"
        values={skills}
        catalog={catalog?.skills}
        saving={saving}
        onChange={setSkills}
      />
      <CatalogIdGroup
        title="Native tools"
        singular="native tool"
        values={nativeTools}
        catalog={catalog?.nativeTools}
        saving={saving}
        onChange={setNativeTools}
      />
      <CatalogIdGroup
        title="Plugin tools"
        singular="plugin tool"
        values={pluginTools}
        catalog={catalog?.pluginTools}
        saving={saving}
        onChange={setPluginTools}
      />
      <SettingsCard>
        <div className="flex items-center gap-3 border-b border-border px-[18px] py-3.5">
          <span className="min-w-0 flex-1">
            <SettingsCardTitle>Apps &amp; MCP</SettingsCardTitle>
          </span>
          <Combobox
            aria-label="App catalog"
            className="w-[220px]"
            placeholder="Enable an app…"
            options={availableToAdd.map((app) => ({ value: app.id, label: app.label, description: app.description || app.id }))}
            value={null}
            disabled={saving || catalog === null || availableToAdd.length === 0}
            onValueChange={(id) => setApps((current) => (current.includes(id) ? current : [...current, id]))}
          />
        </div>
        <div data-testid="agent-app-rows">
          {appRows.length === 0 ? (
            <p className="m-0 px-[18px] py-4 text-xs text-muted-foreground">No apps available.</p>
          ) : (
            appRows.map(({ id, app }) => {
              const enabled = apps.includes(id);
              const available = app?.available === true;
              return (
                <SettingsCardRow key={id} className="gap-3">
                  <span className="min-w-0 flex-1">
                    <span className={`block text-[13px] font-medium${available ? "" : " text-destructive"}`}>
                      {available ? app.label : "Unavailable"}
                    </span>
                    <span className="block text-[11px] text-muted-foreground">{id}</span>
                  </span>
                  {available ? (
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
                      disabled={saving}
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
          disabled={saving || hasUnavailable}
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
