import {
  Button,
  Combobox,
  Input,
  Segmented,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
  Switch,
} from "@ryuzi/ui";
import { AlertTriangle, Copy, Layers, Loader2, MonitorUp, X } from "lucide-react";
import { useEffect, useMemo, useState } from "react";
import { toast } from "sonner";
import { commands, type RuntimeConfigStatusInfo } from "@/bindings";
import { Chip, Pill } from "@/components/common/bits";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { PERM_MODES } from "@/constants";
import { runtimeById, useRuntimes } from "@/store-runtimes";
import { agentAllowed, useApps } from "@/store-apps";
import { useConnections } from "@/store-connections";
import { useEndpoint } from "@/store-endpoint";
import { useModelRoutes } from "@/store-model-routes";
import { useNav } from "@/store-nav";

const WARN = "#F59E0B";

// A single label + Combobox row for the endpoint-config card's model
// pickers — options come from the enabled connections' models.
function ModelSelectRow({
  label,
  value,
  options,
  onChange,
}: {
  label: string;
  value: string;
  options: string[];
  onChange: (v: string) => void;
}) {
  return (
    <div className="flex items-center gap-2.5 px-[18px] py-[7px]">
      <span className="w-[100px] shrink-0 text-[12.5px] font-medium text-muted-foreground">{label}</span>
      <Combobox
        aria-label={label}
        options={options.map((m) => ({ value: m, label: m, mono: true }))}
        value={value || null}
        onValueChange={onChange}
        placeholder="— pick a model —"
        className="min-w-0 flex-1"
      />
    </div>
  );
}

// Runtime detail: real detection state, update banner with the actual install
// command, model/permission/flags configuration, and per-tier model routing.
export function RuntimeDetailView({ id }: { id: string }) {
  const { runtimes, refreshing, refresh, update, setTier, setDefault, updating, updateLog, beginUpdate } = useRuntimes();
  const { apps, loaded: appsLoaded, hydrate: hydrateApps, toggleAgent: toggleAppAgent } = useApps();
  const navigate = useNav((s) => s.navigate);

  // Endpoint-config card (spec §5): apply/reset native CLI configs pointed at
  // Ryuzi's local router, guarded on the server being up + a key existing.
  const [cfg, setCfg] = useState<RuntimeConfigStatusInfo | null>(null);
  const { status: epStatus, keys: epKeys } = useEndpoint();
  const { connections } = useConnections();
  const { routes } = useModelRoutes();
  const modelOptions = useMemo(() => {
    const routeModels = routes.filter((r) => r.enabled && r.targets.length > 0).map((r) => r.name);
    const providerModels = connections.filter((c) => c.enabled).flatMap((c) => c.models.map((m) => `${c.provider}/${m}`));
    return Array.from(new Set([...routeModels, ...providerModels]));
  }, [connections, routes]);
  const [model, setModel] = useState("");
  const [opus, setOpus] = useState("");
  const [sonnet, setSonnet] = useState("");
  const [haiku, setHaiku] = useState("");

  useEffect(() => {
    if (!appsLoaded) void hydrateApps();
  }, [appsLoaded, hydrateApps]);

  useEffect(() => {
    void commands.runtimeConfigStatus(id).then((r) => r.status === "ok" && setCfg(r.data));
    void useEndpoint.getState().hydrate();
    void useConnections.getState().hydrate();
    void useModelRoutes.getState().hydrate();
  }, [id]);

  const agent = runtimeById(runtimes, id);
  if (!agent) {
    return <div className="flex min-h-0 flex-1 items-center justify-center text-[13px] text-muted-foreground">Unknown agent.</div>;
  }

  const installed = agent.binaryPath !== null;
  const isDefault = agent.isDefault;
  // The native runtime runs in-process and reaches providers through the
  // internal router, so the CLI-oriented controls (tier mapping, CLI flags,
  // binary path, endpoint-config file) are meaningless for it — its only real
  // knobs are the default model, permission mode, and app access.
  const isNative = agent.id === "native";
  const hasUpdate =
    installed && agent.latestVersion !== null && agent.installedVersion !== null && agent.latestVersion !== agent.installedVersion;
  const updateCmd = agent.npmPackage ? `npm install -g ${agent.npmPackage}` : null;
  const isUpdating = updating[agent.id] === true;
  const permDesc = PERM_MODES.find((m) => m.id === agent.permMode)?.desc ?? "";

  const endpointBlocked = !epStatus?.running || epKeys.length === 0;
  const noModels = modelOptions.length === 0;

  const applyConfig = async () => {
    const res = await commands.applyRuntimeConfig(id, {
      model: model || modelOptions[0] || "",
      opus: id === "claude" ? opus || null : null,
      sonnet: id === "claude" ? sonnet || null : null,
      haiku: id === "claude" ? haiku || null : null,
      models: modelOptions,
    });
    if (res.status === "ok") {
      setCfg(res.data);
      toast.success("Config applied");
    } else {
      toast.error(res.error.message);
    }
  };

  const resetConfig = async () => {
    if (!window.confirm("Remove Ryuzi settings from this runtime's config?")) return;
    const res = await commands.resetRuntimeConfig(id);
    if (res.status === "ok") {
      setCfg(res.data);
      toast.success("Ryuzi config removed");
    } else {
      toast.error(res.error.message);
    }
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[720px]">
        <BackButton label="Runtime" onClick={() => navigate({ kind: "runtime" })} />

        <DetailHeader
          chip={<Chip initial={agent.initial} color={agent.color} size={44} />}
          title={agent.name}
          titleExtra={isDefault ? <Pill variant="primary">Default</Pill> : undefined}
          sub={installed ? `${agent.connection} · ${agent.binaryPath}` : `${agent.connection} · not installed`}
        >
          {!isDefault && agent.enabled && installed && (
            <Button variant="outline" onClick={() => void setDefault(agent.id)}>
              Make default
            </Button>
          )}
          <Switch
            on={agent.enabled && installed}
            onToggle={() => installed && void update(agent.id, { enabled: !agent.enabled })}
            label="Enabled"
          />
        </DetailHeader>

        {hasUpdate && updateCmd && (
          <Card className="mb-3 flex items-start gap-3 px-[18px] py-3.5">
            <MonitorUp aria-hidden size={16} strokeWidth={2} className="mt-px shrink-0" style={{ color: WARN }} />
            <div className="min-w-0 flex-1">
              <div className="text-[13.5px] font-semibold">
                Update available — {agent.latestVersion} (installed {agent.installedVersion})
              </div>
              <div className="mt-1.5 font-mono text-xs text-muted-foreground">{updateCmd}</div>
              {isUpdating && (
                <pre className="mt-2 max-h-40 overflow-auto rounded-md bg-muted px-2 py-1.5 text-[11px] leading-[1.5] text-muted-foreground">
                  {(updateLog[agent.id] ?? []).slice(-12).join("\n") || "Starting…"}
                </pre>
              )}
            </div>
            <div className="flex shrink-0 flex-col items-stretch gap-1.5">
              <Button disabled={isUpdating} onClick={() => void beginUpdate(agent.id)}>
                {isUpdating ? (
                  <Loader2 aria-hidden size={12} strokeWidth={2} className="size-3 animate-spin" />
                ) : (
                  <MonitorUp aria-hidden size={12} strokeWidth={2} className="size-3" />
                )}
                {isUpdating ? "Updating…" : "Update now"}
              </Button>
              <Button
                variant="outline"
                size="sm"
                onClick={() => {
                  void navigator.clipboard.writeText(updateCmd);
                  toast.success("Update command copied");
                }}
              >
                <Copy aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
                Copy command
              </Button>
            </div>
          </Card>
        )}

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Configuration</CardTitle>
          </CardHeader>
          {!isNative && (
            <div className="flex items-center gap-2.5 px-[18px] pb-1 pt-[11px]">
              <span className="text-[13px] font-medium">Model mapping</span>
              <span className="text-[11.5px] text-muted-foreground">Route each tier to any model or combo</span>
            </div>
          )}
          {!isNative &&
            agent.tiers.map((tier) => (
              <div key={tier.id} className="relative flex items-center gap-2.5 px-[18px] py-[7px]">
                <span className="w-[100px] shrink-0 text-[12.5px] font-medium text-muted-foreground">{tier.label}</span>
                <div className="flex h-8 min-w-0 flex-1 items-center gap-2 rounded-md border border-input bg-background pl-3 pr-1">
                  {tier.combo && (
                    <span className="flex shrink-0 items-center gap-1 rounded-full bg-secondary px-1.5 py-[2px] text-[9.5px] font-semibold uppercase tracking-[0.03em] text-secondary-foreground">
                      <Layers aria-hidden size={9} strokeWidth={2} />
                      combo
                    </span>
                  )}
                  {tier.value !== null ? (
                    <>
                      <span className="min-w-0 flex-1 truncate font-mono text-xs">{tier.value}</span>
                      <Button
                        variant="ghost"
                        size="icon-xs"
                        title="Clear"
                        aria-label={`Clear ${tier.label} model`}
                        onClick={() => void setTier(agent.id, tier.id, null)}
                        className="text-muted-foreground"
                      >
                        <X aria-hidden size={12} strokeWidth={2} />
                      </Button>
                    </>
                  ) : (
                    <span className="min-w-0 flex-1 font-mono text-xs text-muted-foreground">Not set</span>
                  )}
                </div>
                <Combobox
                  aria-label={`${tier.label} model`}
                  options={[
                    ...agent.models.map((m) => ({ value: m, label: m, mono: true })),
                    { value: "__combo__", label: "Route by task (combo)" },
                  ]}
                  value={tier.combo ? "__combo__" : tier.value}
                  onValueChange={(v) => {
                    if (v === "__combo__") void setTier(agent.id, tier.id, "route by task", true);
                    else void setTier(agent.id, tier.id, v);
                  }}
                  trigger={
                    <Button
                      variant="outline"
                      title={
                        agent.models.length === 0
                          ? `No models detected${agent.id === "ollama" ? " — pull one with `ollama pull`" : ""}.`
                          : undefined
                      }
                    >
                      Select model
                    </Button>
                  }
                />
              </div>
            ))}
          <div className="h-2 border-b border-border" />
          <div className="flex flex-col gap-2 border-b border-border px-[18px] py-3">
            <div className="flex items-center gap-3">
              <span className="flex-1 text-[13px] font-medium">Permission mode</span>
              <Segmented
                options={PERM_MODES.map((m) => ({ id: m.id, label: m.label }))}
                value={agent.permMode as (typeof PERM_MODES)[number]["id"]}
                onChange={(mode) => void update(agent.id, { permMode: mode })}
              />
            </div>
            <div className="text-right text-[11.5px] text-muted-foreground">{permDesc}</div>
          </div>
          <CardRow>
            <span className="w-[110px] shrink-0 text-[13px] font-medium">Default model</span>
            {isNative ? (
              agent.models.length > 0 ? (
                <Combobox
                  aria-label="Default model"
                  options={[
                    { value: "", label: "Router default (first usable provider)" },
                    ...agent.models.map((m) => ({ value: m, label: m, mono: true })),
                  ]}
                  value={agent.model || ""}
                  onValueChange={(v) => void update(agent.id, { model: v })}
                  className="min-w-0 flex-1"
                />
              ) : (
                <span className="flex-1 truncate text-xs text-muted-foreground">
                  Add an enabled provider connection in Models → Providers to pick a model.
                </span>
              )
            ) : (
              <span className="flex-1 truncate font-mono text-xs text-muted-foreground">{agent.model || "engine default"}</span>
            )}
          </CardRow>
          {!isNative && (
            <CardRow>
              <span className="w-[110px] shrink-0 text-[13px] font-medium">CLI flags</span>
              <Input
                defaultValue={agent.flags}
                onBlur={(e) => {
                  if (e.target.value !== agent.flags) void update(agent.id, { flags: e.target.value });
                }}
                placeholder="No extra flags"
                className="flex-1 font-mono text-xs md:text-xs"
              />
            </CardRow>
          )}
          {!isNative && (
            <CardRow>
              <span className="w-[110px] shrink-0 text-[13px] font-medium">Binary</span>
              <span className="flex-1 truncate font-mono text-xs text-muted-foreground">{agent.binaryPath ?? "not found on PATH"}</span>
            </CardRow>
          )}
        </Card>

        {!isNative && (
          <Card className="mb-3">
            <CardHeader>
              <CardTitle>Endpoint config</CardTitle>
              <Pill variant={cfg?.configured ? "primary" : "secondary"}>{cfg?.configured ? "Configured" : "Not configured"}</Pill>
            </CardHeader>
            {cfg?.configPath && <div className="px-[18px] pb-1 pt-3 font-mono text-[11px] text-muted-foreground">{cfg.configPath}</div>}
            {cfg && !cfg.supported ? (
              <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">
                Config apply for this runtime is coming in a later phase.
              </div>
            ) : (
              <>
                {endpointBlocked && (
                  <div
                    className="mx-[18px] mt-3 flex items-start gap-2 rounded-md border px-3 py-2 text-[12px]"
                    style={{ borderColor: WARN, color: WARN }}
                  >
                    <AlertTriangle aria-hidden size={14} strokeWidth={2} className="mt-px shrink-0" />
                    <span>Start the endpoint server and create an API key in Models → Endpoint before applying.</span>
                  </div>
                )}
                {noModels && (
                  <div
                    className="mx-[18px] mt-3 flex items-start gap-2 rounded-md border px-3 py-2 text-[12px]"
                    style={{ borderColor: WARN, color: WARN }}
                  >
                    <AlertTriangle aria-hidden size={14} strokeWidth={2} className="mt-px shrink-0" />
                    <span>Add an enabled provider connection in Models → Providers to pick models.</span>
                  </div>
                )}
                <div className="flex flex-col gap-1 py-2">
                  {agent.id === "claude" && (
                    <>
                      <ModelSelectRow label="Opus" value={opus} options={modelOptions} onChange={setOpus} />
                      <ModelSelectRow label="Sonnet" value={sonnet} options={modelOptions} onChange={setSonnet} />
                      <ModelSelectRow label="Haiku" value={haiku} options={modelOptions} onChange={setHaiku} />
                    </>
                  )}
                  <ModelSelectRow label="Default model" value={model} options={modelOptions} onChange={setModel} />
                </div>
                <div className="flex items-center justify-end gap-2 border-t border-border px-[18px] py-3">
                  {cfg?.configured && (
                    <Button variant="outline" onClick={() => void resetConfig()}>
                      Reset
                    </Button>
                  )}
                  <Button disabled={endpointBlocked || noModels} onClick={() => void applyConfig()}>
                    Apply
                  </Button>
                </div>
              </>
            )}
          </Card>
        )}

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>App access</CardTitle>
            <CardHint>Which installed apps this agent may call</CardHint>
          </CardHeader>
          {apps.length === 0 && (
            <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">
              No plugins installed yet — add MCP servers from the Plugins screen.
            </div>
          )}
          {apps.map((app) => (
            <CardRow key={app.id} className="py-[11px]">
              <Chip initial={app.initial} color={app.color} size={28} mono />
              <span className="min-w-0 flex-1">
                <span className="block text-[13px] font-medium">{app.name}</span>
                <span className="block text-[11px] text-muted-foreground">{app.kind}</span>
              </span>
              <Switch
                on={agentAllowed(app, agent.id)}
                onToggle={() => void toggleAppAgent(app.id, agent.id, !agentAllowed(app, agent.id))}
                label={`${app.name} access`}
              />
            </CardRow>
          ))}
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Version</CardTitle>
            <span className="font-mono text-[11.5px] text-muted-foreground">
              {installed
                ? `${agent.installedVersion ?? "unknown"} installed${agent.latestVersion ? ` · ${agent.latestVersion} latest` : ""}`
                : "not installed"}
            </span>
            <span className="flex-1" />
            <Button variant="outline" size="sm" onClick={() => void refresh()} disabled={refreshing}>
              {refreshing ? "Checking…" : "Check for updates"}
            </Button>
          </CardHeader>
          <div className="px-[18px] py-3 text-[12.5px] text-muted-foreground">
            {agent.npmPackage ? (
              <>
                Published as <span className="font-mono text-xs">{agent.npmPackage}</span> on npm.
                {!hasUpdate && installed && agent.latestVersion ? " You're up to date." : ""}
              </>
            ) : (
              "Version updates are managed by the agent's own installer."
            )}
          </div>
        </Card>
      </div>
    </div>
  );
}
