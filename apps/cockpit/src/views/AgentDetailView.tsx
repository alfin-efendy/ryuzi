import { Layers, MonitorUp, X } from "lucide-react";
import { useState } from "react";
import { Chip, Pill } from "@/components/common/bits";
import { Card, CardHeader, CardHint, CardRow, CardTitle } from "@/components/common/Card";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { MenuItem, MenuPanel, MenuSeparator } from "@/components/common/MenuPanel";
import { Segmented } from "@/components/common/Segmented";
import { Switch } from "@/components/common/Switch";
import { type AgentId, AGENTS, PERM_MODES } from "@/fixtures";
import { useFixtures } from "@/store-fixtures";
import { useNav } from "@/store-nav";

const WARN = "#F59E0B";

function ChangelogNote({ note }: { note: string }) {
  return (
    <div className="flex items-baseline gap-2 text-[12.5px] text-muted-foreground">
      <span className="h-1 w-1 shrink-0 -translate-y-[2px] rounded-full bg-muted-foreground" />
      {note}
    </div>
  );
}

// Agent detail: update banner, model/permission/flags configuration, per-app
// access toggles, and the installed version with its changelog.
export function AgentDetailView({ id }: { id: string }) {
  const agentId = id as AgentId;
  const agent = AGENTS[agentId];
  const {
    defaultAgent,
    agentState,
    apps,
    setDefaultAgent,
    toggleAgent,
    setAgentPerm,
    setAgentFlags,
    applyAgentUpdate,
    setAgentAppAccess,
    setAgentTier,
  } = useFixtures();
  const navigate = useNav((s) => s.navigate);
  const [openTierMenu, setOpenTierMenu] = useState<string | null>(null);

  const st = agentState[agentId];
  const isDefault = defaultAgent === agentId;
  const hasUpdate = st.version !== agent.latest;
  const updateNotes = agent.changelog.find((c) => c.v === agent.latest)?.notes ?? [];
  const permDesc = PERM_MODES.find((m) => m.id === st.permMode)?.desc ?? "";

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[720px]">
        <BackButton label="Agents" onClick={() => navigate({ kind: "agents" })} />

        <DetailHeader
          chip={<Chip initial={agent.initial} color={agent.color} size={44} />}
          title={agent.name}
          titleExtra={isDefault ? <Pill variant="primary">Default</Pill> : undefined}
          sub={`${agent.connection} · ${agent.binary}`}
        >
          {!isDefault && st.enabled && (
            <button
              type="button"
              onClick={() => setDefaultAgent(agentId)}
              className="h-8 shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-3 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent"
            >
              Make default
            </button>
          )}
          <Switch on={st.enabled} onToggle={() => toggleAgent(agentId)} label="Enabled" />
        </DetailHeader>

        {hasUpdate && (
          <Card className="mb-3 flex items-start gap-3 px-[18px] py-3.5">
            <MonitorUp aria-hidden size={16} strokeWidth={2} className="mt-px shrink-0" style={{ color: WARN }} />
            <div className="min-w-0 flex-1">
              <div className="text-[13.5px] font-semibold">Update available — {agent.latest}</div>
              <div className="mt-1.5 flex flex-col gap-[2px]">
                {updateNotes.map((n) => (
                  <ChangelogNote key={n} note={n} />
                ))}
              </div>
            </div>
            <button
              type="button"
              onClick={() => applyAgentUpdate(agentId)}
              className="h-[30px] shrink-0 cursor-pointer rounded-md border-none bg-primary px-3.5 font-sans text-[12.5px] font-medium text-primary-foreground hover:opacity-85"
            >
              Update now
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
          {st.tiers.map((tier) => (
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
                      onClick={() => setAgentTier(agentId, tier.id, null)}
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
                  {agent.models.map((m) => (
                    <MenuItem
                      key={m}
                      selected={!tier.combo && tier.value === m}
                      onClick={() => {
                        setAgentTier(agentId, tier.id, m);
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
                      setAgentTier(agentId, tier.id, "route by task", true);
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
                value={st.permMode}
                onChange={(mode) => setAgentPerm(agentId, mode)}
              />
            </div>
            <div className="text-right text-[11.5px] text-muted-foreground">{permDesc}</div>
          </div>
          <CardRow>
            <span className="w-[110px] shrink-0 text-[13px] font-medium">CLI flags</span>
            <input
              value={st.flags}
              onChange={(e) => setAgentFlags(agentId, e.target.value)}
              placeholder="No extra flags"
              className="h-8 min-w-0 flex-1 rounded-md border border-input bg-background px-3 font-mono text-xs text-foreground"
            />
          </CardRow>
          <CardRow>
            <span className="w-[110px] shrink-0 text-[13px] font-medium">Binary</span>
            <span className="flex-1 truncate font-mono text-xs text-muted-foreground">{agent.binary}</span>
            <button
              type="button"
              className="h-[27px] shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-[11px] font-sans text-xs font-medium text-foreground hover:bg-accent"
            >
              Reveal
            </button>
          </CardRow>
        </Card>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>App access</CardTitle>
            <CardHint>Which installed apps this agent may call</CardHint>
          </CardHeader>
          {apps.map((app) => (
            <CardRow key={app.id} className="py-[11px]">
              <Chip initial={app.initial} color={app.color} size={28} mono />
              <span className="min-w-0 flex-1">
                <span className="block text-[13px] font-medium">{app.name}</span>
                <span className="block text-[11px] text-muted-foreground">{app.kind}</span>
              </span>
              <Switch
                on={app.agentAccess[agentId]}
                onToggle={() => setAgentAppAccess(agentId, app.id, !app.agentAccess[agentId])}
                label={`${app.name} access`}
              />
            </CardRow>
          ))}
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Version</CardTitle>
            <span className="font-mono text-[11.5px] text-muted-foreground">{st.version} installed</span>
            <span className="flex-1" />
            <button
              type="button"
              className="h-[27px] shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-[11px] font-sans text-xs font-medium text-foreground hover:bg-accent"
            >
              Check for updates
            </button>
          </CardHeader>
          {agent.changelog.map((cl) => (
            <div key={cl.v} className="border-b border-border px-[18px] py-3 last:border-b-0">
              <div className="flex items-baseline gap-2.5">
                <span className="font-mono text-[12.5px] font-semibold">{cl.v}</span>
                <span className="text-[11.5px] text-muted-foreground">{cl.date}</span>
              </div>
              <div className="mt-[5px] flex flex-col gap-[2px]">
                {cl.notes.map((n) => (
                  <ChangelogNote key={n} note={n} />
                ))}
              </div>
            </div>
          ))}
        </Card>
      </div>
    </div>
  );
}
