import { useState } from "react";
import { Check, Minus, Plus } from "lucide-react";
import { Card } from "@/components/common/Card";
import { Segmented } from "@/components/common/Segmented";
import { Chip, Pill, StatusDot } from "@/components/common/bits";
import { AGENT_IDS, AGENTS, type AgentId, type AppFixture, WORKSPACES } from "@/fixtures";
import { useFixtures } from "@/store-fixtures";
import { useNav } from "@/store-nav";

const AGENT_SHORT: Record<AgentId, string> = { claude: "Claude", codex: "Codex", gemini: "Gemini", openclaw: "Claw", local: "Local" };

// App name column + one centered toggle column per agent.
const MATRIX_GRID = "grid-cols-[minmax(0,1fr)_repeat(5,72px)]";

function appStatus(app: AppFixture): { color: string; label: string } {
  return app.status === "error" ? { color: "#EF4444", label: "Error" } : { color: "#22C55E", label: "Connected" };
}

function scopeLabel(app: AppFixture): string {
  if (app.scope === "global") return "Global";
  const names = WORKSPACES.filter((w) => app.scopeWs[w.id]).map((w) => w.name);
  return names.length > 0 ? names.join(", ") : "—";
}

export function AppsView() {
  const nav = useNav();
  const { apps, toggleAppAgent } = useFixtures();
  const [tab, setTab] = useState<"installed" | "access">("installed");

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <div className="mb-5 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Apps</h2>
            <p className="m-0 text-[13px] text-muted-foreground">
              Tools and MCP servers your agents can call. Installed per workspace gateway.
            </p>
          </div>
          <button
            type="button"
            onClick={() => nav.navigate({ kind: "registry" })}
            className="flex h-8 shrink-0 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-3 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent"
          >
            <Plus aria-hidden size={14} strokeWidth={2} />
            Browse registry
          </button>
        </div>

        <div className="mb-4">
          <Segmented
            options={[
              { id: "installed", label: "Installed" },
              { id: "access", label: "Access" },
            ]}
            value={tab}
            onChange={setTab}
          />
        </div>

        {tab === "installed" && (
          <div className="grid grid-cols-2 gap-3">
            {apps.map((app) => {
              const status = appStatus(app);
              const open = () => nav.navigate({ kind: "appDetail", id: app.id });
              return (
                <Card key={app.id} className="flex flex-col gap-3 px-[18px] py-4">
                  <button
                    type="button"
                    onClick={open}
                    className="flex cursor-pointer items-center gap-3 border-none bg-transparent p-0 text-left font-sans text-foreground"
                  >
                    <Chip initial={app.initial} color={app.color} size={38} mono />
                    <span className="min-w-0 flex-1">
                      <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-sm font-semibold">{app.name}</span>
                      <span className="block overflow-hidden text-ellipsis whitespace-nowrap text-[11.5px] text-muted-foreground">
                        {app.kind}
                      </span>
                    </span>
                    <span className="flex shrink-0 items-center gap-[5px] text-[11px] text-muted-foreground">
                      <StatusDot color={status.color} />
                      {status.label}
                    </span>
                  </button>
                  <p className="m-0 text-[12.5px] leading-[1.5] text-muted-foreground">{app.desc}</p>
                  <div className="flex items-center gap-2 pt-0.5">
                    <Pill variant="mono">{scopeLabel(app)}</Pill>
                    <span className="flex-1" />
                    <button
                      type="button"
                      onClick={open}
                      className="h-[27px] cursor-pointer rounded-md border border-border bg-transparent px-[11px] font-sans text-xs font-medium text-foreground hover:bg-accent"
                    >
                      Configure
                    </button>
                  </div>
                </Card>
              );
            })}
          </div>
        )}

        {tab === "access" && (
          <>
            <Card>
              <div className={`grid ${MATRIX_GRID} items-center border-b border-border px-[18px] py-2.5`}>
                <span className="text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground">App</span>
                {AGENT_IDS.map((aid) => (
                  <span key={aid} className="flex items-center justify-center gap-1.5 text-[11.5px] font-semibold">
                    <StatusDot color={AGENTS[aid].color} />
                    {AGENT_SHORT[aid]}
                  </span>
                ))}
              </div>
              {apps.map((app) => (
                <div key={app.id} className={`grid ${MATRIX_GRID} items-center border-b border-border px-[18px] py-[9px] last:border-b-0`}>
                  <span className="flex min-w-0 items-center gap-2.5">
                    <Chip initial={app.initial} color={app.color} size={26} mono />
                    <span className="overflow-hidden text-ellipsis whitespace-nowrap text-[13px] font-medium">{app.name}</span>
                  </span>
                  {AGENT_IDS.map((aid) => {
                    const on = !!app.agentAccess[aid];
                    return (
                      <span key={aid} className="flex justify-center">
                        <button
                          type="button"
                          aria-label={`${on ? "Block" : "Allow"} ${app.name} for ${AGENT_SHORT[aid]}`}
                          onClick={() => toggleAppAgent(app.id, aid)}
                          className="flex h-[26px] w-[26px] cursor-pointer items-center justify-center rounded-sm border border-border bg-transparent p-0 text-muted-foreground hover:bg-accent"
                        >
                          {on ? (
                            <Check aria-hidden size={13} strokeWidth={2.5} style={{ color: "#22C55E" }} />
                          ) : (
                            <Minus aria-hidden size={11} strokeWidth={2} />
                          )}
                        </button>
                      </span>
                    );
                  })}
                </div>
              ))}
            </Card>
            <p className="mx-0.5 mb-0 mt-2.5 text-xs text-muted-foreground">
              Access here applies before per-tool permissions — a blocked agent never sees the app’s tools.
            </p>
          </>
        )}
      </div>
    </div>
  );
}
