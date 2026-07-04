import { cn } from "@ryuzi/ui";
import { ChevronRight, RefreshCw } from "lucide-react";
import { Chip, Pill, StatusDot } from "@/components/common/bits";
import { Card } from "@/components/common/Card";
import { Switch } from "@/components/common/Switch";
import { useAgents } from "@/store-agents";
import { useNav } from "@/store-nav";

const SUCCESS = "#22C55E";

// Agents settings list: every CLI agent Cockpit can drive, with real binary
// detection, default/update badges, enable toggles, and a detail screen path.
export function AgentsView() {
  const { agents, refreshing, refresh, update, setDefault } = useAgents();
  const navigate = useNav((s) => s.navigate);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[720px]">
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Agents</h2>
            <p className="m-0 mb-5 text-[13px] text-muted-foreground">
              Cockpit is agent-agnostic. Any CLI coding agent can run a session — mix them across projects.
            </p>
          </div>
          <button
            type="button"
            onClick={() => void refresh()}
            disabled={refreshing}
            className="flex h-8 shrink-0 cursor-pointer items-center gap-2 rounded-md border border-border bg-transparent px-3 font-sans text-xs font-medium text-foreground hover:bg-accent disabled:opacity-50"
          >
            <RefreshCw aria-hidden size={13} strokeWidth={2} className={refreshing ? "animate-spin" : ""} />
            {refreshing ? "Detecting…" : "Re-detect"}
          </button>
        </div>
        <div className="flex flex-col gap-3">
          {agents.map((agent) => {
            const installed = agent.binaryPath !== null;
            const isDefault = agent.isDefault;
            const hasUpdate =
              installed &&
              agent.latestVersion !== null &&
              agent.installedVersion !== null &&
              agent.latestVersion !== agent.installedVersion;
            const statusColor = installed ? SUCCESS : "var(--muted-foreground)";
            const open = () => navigate({ kind: "agentDetail", id: agent.id });
            return (
              <Card key={agent.id} className={cn("flex items-center gap-3.5 px-[18px] py-4", isDefault && "border-ring")}>
                <Chip initial={agent.initial} color={agent.color} size={36} onClick={open} />
                <button
                  type="button"
                  onClick={open}
                  className="min-w-0 flex-1 cursor-pointer border-none bg-transparent p-0 text-left font-sans"
                >
                  <span className="flex items-center gap-2">
                    <span className="text-sm font-semibold text-foreground">{agent.name}</span>
                    {isDefault && <Pill variant="primary">Default</Pill>}
                    {hasUpdate && <Pill variant="warn">Update {agent.latestVersion}</Pill>}
                  </span>
                  <span className="mt-0.5 block text-[12.5px] text-muted-foreground">
                    {installed ? `${agent.model || agent.connection} · ${agent.connection}` : "Not installed"}
                  </span>
                </button>
                <span className="flex shrink-0 items-center gap-1.5 text-xs" style={{ color: statusColor }}>
                  <StatusDot color={statusColor} />
                  {installed ? (agent.installedVersion ? `v${agent.installedVersion}` : "Installed") : "Not found"}
                </span>
                {!isDefault && agent.enabled && installed && (
                  <button
                    type="button"
                    onClick={() => void setDefault(agent.id)}
                    className="h-7 shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-3 font-sans text-xs font-medium text-foreground hover:bg-accent"
                  >
                    Make default
                  </button>
                )}
                <Switch
                  on={agent.enabled && installed}
                  onToggle={() => installed && void update(agent.id, { enabled: !agent.enabled })}
                  label={`${agent.name} enabled`}
                />
                <button
                  type="button"
                  onClick={open}
                  title="Details"
                  className="flex size-7 shrink-0 cursor-pointer items-center justify-center rounded-md border-none bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground"
                >
                  <ChevronRight aria-hidden size={14} strokeWidth={2} />
                </button>
              </Card>
            );
          })}
        </div>
      </div>
    </div>
  );
}
