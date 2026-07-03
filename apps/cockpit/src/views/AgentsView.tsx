import { cn } from "@ryuzi/ui";
import { ChevronRight, Plus } from "lucide-react";
import { Chip, Pill, StatusDot } from "@/components/common/bits";
import { Card } from "@/components/common/Card";
import { Switch } from "@/components/common/Switch";
import { AGENT_IDS, AGENTS } from "@/fixtures";
import { useFixtures } from "@/store-fixtures";
import { useNav } from "@/store-nav";

const SUCCESS = "#22C55E";

// Agents settings list: every CLI agent Cockpit can drive, with default/update
// badges, connection status, enable toggles, and a path into the detail screen.
export function AgentsView() {
  const { defaultAgent, agentState, setDefaultAgent, toggleAgent } = useFixtures();
  const navigate = useNav((s) => s.navigate);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[720px]">
        <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Agents</h2>
        <p className="m-0 mb-5 text-[13px] text-muted-foreground">
          Cockpit is agent-agnostic. Any CLI coding agent can run a session — mix them across projects.
        </p>
        <div className="flex flex-col gap-3">
          {AGENT_IDS.map((id) => {
            const agent = AGENTS[id];
            const st = agentState[id];
            const isDefault = defaultAgent === id;
            const hasUpdate = st.version !== agent.latest;
            const connected = st.enabled || id !== "local";
            const statusColor = connected ? SUCCESS : "var(--muted-foreground)";
            const open = () => navigate({ kind: "agentDetail", id });
            return (
              <Card key={id} className={cn("flex items-center gap-3.5 px-[18px] py-4", isDefault && "border-ring")}>
                <Chip initial={agent.initial} color={agent.color} size={36} onClick={open} />
                <button
                  type="button"
                  onClick={open}
                  className="min-w-0 flex-1 cursor-pointer border-none bg-transparent p-0 text-left font-sans"
                >
                  <span className="flex items-center gap-2">
                    <span className="text-sm font-semibold text-foreground">{agent.name}</span>
                    {isDefault && <Pill variant="primary">Default</Pill>}
                    {hasUpdate && <Pill variant="warn">Update {agent.latest}</Pill>}
                  </span>
                  <span className="mt-0.5 block text-[12.5px] text-muted-foreground">
                    {st.model} · {agent.connection}
                  </span>
                </button>
                <span className="flex shrink-0 items-center gap-1.5 text-xs" style={{ color: statusColor }}>
                  <StatusDot color={statusColor} />
                  {connected ? "Connected" : "Not running"}
                </span>
                {!isDefault && st.enabled && (
                  <button
                    type="button"
                    onClick={() => setDefaultAgent(id)}
                    className="h-7 shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-3 font-sans text-xs font-medium text-foreground hover:bg-accent"
                  >
                    Make default
                  </button>
                )}
                <Switch on={st.enabled} onToggle={() => toggleAgent(id)} label={`${agent.name} enabled`} />
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
        <button
          type="button"
          className="mt-4 flex w-full cursor-pointer items-center gap-3 rounded-xl border border-dashed border-border bg-transparent px-[18px] py-[15px] font-sans text-muted-foreground hover:bg-accent hover:text-accent-foreground"
        >
          <Plus aria-hidden size={16} strokeWidth={2} />
          <span className="text-[13px] font-medium">Add an agent — point Cockpit at any CLI binary</span>
        </button>
      </div>
    </div>
  );
}
