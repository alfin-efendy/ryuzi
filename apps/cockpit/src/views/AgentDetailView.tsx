import { Copy, Layers, MonitorUp, X } from "lucide-react";
import { useEffect, useState } from "react";
import { toast } from "sonner";
import { Chip, Pill } from "@/components/common/bits";
import { Card, CardHeader, CardHint, CardRow, CardTitle } from "@/components/common/Card";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { MenuItem, MenuPanel, MenuSeparator } from "@/components/common/MenuPanel";
import { Segmented } from "@/components/common/Segmented";
import { Switch } from "@/components/common/Switch";
import { PERM_MODES } from "@/constants";
import { agentById, useAgents } from "@/store-agents";
import { agentAllowed, useApps } from "@/store-apps";
import { useNav } from "@/store-nav";

const WARN = "#F59E0B";

// Agent detail: real detection state, update banner with the actual install
// command, model/permission/flags configuration, and per-tier model routing.
export function AgentDetailView({ id }: { id: string }) {
  const { agents, refreshing, refresh, update, setTier, setDefault } = useAgents();
  const { apps, loaded: appsLoaded, hydrate: hydrateApps, toggleAgent: toggleAppAgent } = useApps();
  const navigate = useNav((s) => s.navigate);
  const [openTierMenu, setOpenTierMenu] = useState<string | null>(null);

  useEffect(() => {
    if (!appsLoaded) void hydrateApps();
  }, [appsLoaded, hydrateApps]);

  const agent = agentById(agents, id);
  if (!agent) {
    return <div className="flex min-h-0 flex-1 items-center justify-center text-[13px] text-muted-foreground">Unknown agent.</div>;
  }

  const installed = agent.binaryPath !== null;
  const isDefault = agent.isDefault;
  const hasUpdate =
    installed && agent.latestVersion !== null && agent.installedVersion !== null && agent.latestVersion !== agent.installedVersion;
  const updateCmd = agent.npmPackage ? `npm install -g ${agent.npmPackage}` : null;
  const permDesc = PERM_MODES.find((m) => m.id === agent.permMode)?.desc ?? "";

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[720px]">
        <BackButton label="Agents" onClick={() => navigate({ kind: "agents" })} />

        <DetailHeader
          chip={<Chip initial={agent.initial} color={agent.color} size={44} />}
          title={agent.name}
          titleExtra={isDefault ? <Pill variant="primary">Default</Pill> : undefined}
          sub={installed ? `${agent.connection} · ${agent.binaryPath}` : `${agent.connection} · not installed`}
        >
          {!isDefault && agent.enabled && installed && (
            <button
              type="button"
              onClick={() => void setDefault(agent.id)}
              className="h-8 shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-3 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent"
            >
              Make default
            </button>
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
            </div>
            <button
              type="button"
              onClick={() => {
                void navigator.clipboard.writeText(updateCmd);
                toast.success("Update command copied");
              }}
              className="flex h-[30px] shrink-0 cursor-pointer items-center gap-1.5 rounded-md border-none bg-primary px-3.5 font-sans text-[12.5px] font-medium text-primary-foreground hover:opacity-85"
            >
              <Copy aria-hidden size={12} strokeWidth={2} />
              Copy command
            </button>
          </Card>
        )}

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Configuration</CardTitle>
          </CardHeader>
          <div className="flex items-center gap-2.5 px-[18px] pb-1 pt-[11px]">
            <span className="text-[13px] font-medium">Model mapping</span>
            <span className="text-[11.5px] text-muted-foreground">Route each tier to any model or combo</span>
          </div>
          {agent.tiers.map((tier) => (
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
                    <button
                      type="button"
                      title="Clear"
                      aria-label={`Clear ${tier.label} model`}
                      onClick={() => void setTier(agent.id, tier.id, null)}
                      className="flex h-6 w-6 shrink-0 cursor-pointer items-center justify-center rounded-sm border-none bg-transparent p-0 text-muted-foreground hover:bg-accent hover:text-accent-foreground"
                    >
                      <X aria-hidden size={12} strokeWidth={2} />
                    </button>
                  </>
                ) : (
                  <span className="min-w-0 flex-1 font-mono text-xs text-muted-foreground">Not set</span>
                )}
              </div>
              <button
                type="button"
                onClick={() => setOpenTierMenu((v) => (v === tier.id ? null : tier.id))}
                className="h-8 shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-3 font-sans text-xs font-medium text-foreground hover:bg-accent"
              >
                Select model
              </button>
              {openTierMenu === tier.id && (
                <MenuPanel onClose={() => setOpenTierMenu(null)} className="right-[18px] top-[42px] w-[240px]">
                  {agent.models.length === 0 && (
                    <div className="px-3 py-2 text-[12px] text-muted-foreground">
                      No models detected{agent.id === "ollama" ? " — pull one with `ollama pull`" : ""}.
                    </div>
                  )}
                  {agent.models.map((m) => (
                    <MenuItem
                      key={m}
                      selected={!tier.combo && tier.value === m}
                      onClick={() => {
                        void setTier(agent.id, tier.id, m);
                        setOpenTierMenu(null);
                      }}
                    >
                      <span className="flex-1">{m}</span>
                    </MenuItem>
                  ))}
                  <MenuSeparator />
                  <MenuItem
                    selected={tier.combo === true}
                    onClick={() => {
                      void setTier(agent.id, tier.id, "route by task", true);
                      setOpenTierMenu(null);
                    }}
                  >
                    <span className="flex-1">Route by task (combo)</span>
                  </MenuItem>
                </MenuPanel>
              )}
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
            <span className="flex-1 truncate font-mono text-xs text-muted-foreground">{agent.model || "engine default"}</span>
          </CardRow>
          <CardRow>
            <span className="w-[110px] shrink-0 text-[13px] font-medium">CLI flags</span>
            <input
              defaultValue={agent.flags}
              onBlur={(e) => {
                if (e.target.value !== agent.flags) void update(agent.id, { flags: e.target.value });
              }}
              placeholder="No extra flags"
              className="h-8 min-w-0 flex-1 rounded-md border border-input bg-background px-3 font-mono text-xs text-foreground"
            />
          </CardRow>
          <CardRow>
            <span className="w-[110px] shrink-0 text-[13px] font-medium">Binary</span>
            <span className="flex-1 truncate font-mono text-xs text-muted-foreground">{agent.binaryPath ?? "not found on PATH"}</span>
          </CardRow>
        </Card>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>App access</CardTitle>
            <CardHint>Which installed apps this agent may call</CardHint>
          </CardHeader>
          {apps.length === 0 && (
            <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">
              No apps installed yet — add MCP servers from the Apps screen.
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
            <button
              type="button"
              onClick={() => void refresh()}
              disabled={refreshing}
              className="h-[27px] shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-[11px] font-sans text-xs font-medium text-foreground hover:bg-accent disabled:opacity-50"
            >
              {refreshing ? "Checking…" : "Check for updates"}
            </button>
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
