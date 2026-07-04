import { useEffect, useState } from "react";
import { Check, Minus, Plus, Store } from "lucide-react";
import { Card } from "@/components/common/Card";
import { Segmented } from "@/components/common/Segmented";
import { Chip, Pill, StatusDot } from "@/components/common/bits";
import type { AppInfo } from "@/bindings";
import { agentAllowed, useApps } from "@/store-apps";
import { useAgents } from "@/store-agents";
import { useGateways } from "@/store-gateways";
import { AddAppModal } from "@/components/modals/AddAppModal";
import { useNav } from "@/store-nav";

// App name column + one centered toggle column per agent.
const matrixGrid = (n: number) => `minmax(0,1fr) repeat(${n}, 72px)`;

function appStatus(app: AppInfo): { color: string; label: string } {
  if (app.status === "connected") return { color: "#22C55E", label: "Connected" };
  if (app.status === "error") return { color: "#EF4444", label: "Error" };
  return { color: "var(--muted-foreground)", label: "Unchecked" };
}

export function AppsView() {
  const nav = useNav();
  const { apps, loaded, hydrate, toggleAgent } = useApps();
  const agents = useAgents((s) => s.agents);
  const gateways = useGateways((s) => s.gateways);
  const [tab, setTab] = useState<"installed" | "access">("installed");
  const [addOpen, setAddOpen] = useState(false);

  useEffect(() => {
    void hydrate();
  }, [hydrate]);

  const scopeLabel = (app: AppInfo): string => {
    if (app.scope === "global") return "Global";
    const names = gateways.filter((w) => app.scopeGateways.includes(w.id)).map((w) => w.name);
    return names.length > 0 ? names.join(", ") : "—";
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[860px]">
        <div className="mb-5 flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Apps</h2>
            <p className="m-0 text-[13px] text-muted-foreground">
              Tools and MCP servers your agents can call — attached to every session they're allowed in.
            </p>
          </div>
          <button
            type="button"
            onClick={() => setAddOpen(true)}
            className="flex h-8 shrink-0 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-3 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent"
          >
            <Plus aria-hidden size={14} strokeWidth={2} />
            Add app
          </button>
          <button
            type="button"
            onClick={() => nav.navigate({ kind: "registry" })}
            className="flex h-8 shrink-0 cursor-pointer items-center gap-[7px] rounded-md border-none bg-primary px-3 font-sans text-[12.5px] font-medium text-primary-foreground hover:opacity-90"
          >
            <Store aria-hidden size={14} strokeWidth={2} />
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

        {loaded && apps.length === 0 && (
          <Card className="p-6 text-center text-[13px] text-muted-foreground">
            No apps installed yet. Add an MCP server by hand or browse the registry.
          </Card>
        )}

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
                  <p className="m-0 text-[12.5px] leading-[1.5] text-muted-foreground">{app.desc || "No description."}</p>
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

        {tab === "access" && apps.length > 0 && (
          <>
            <Card>
              <div
                className="grid items-center border-b border-border px-[18px] py-2.5"
                style={{ gridTemplateColumns: matrixGrid(agents.length) }}
              >
                <span className="text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground">App</span>
                {agents.map((a) => (
                  <span key={a.id} className="flex items-center justify-center gap-1.5 text-[11.5px] font-semibold">
                    <StatusDot color={a.color} />
                    {a.name.split(" ")[0]}
                  </span>
                ))}
              </div>
              {apps.map((app) => (
                <div
                  key={app.id}
                  className="grid items-center border-b border-border px-[18px] py-[9px] last:border-b-0"
                  style={{ gridTemplateColumns: matrixGrid(agents.length) }}
                >
                  <span className="flex min-w-0 items-center gap-2.5">
                    <Chip initial={app.initial} color={app.color} size={26} mono />
                    <span className="overflow-hidden text-ellipsis whitespace-nowrap text-[13px] font-medium">{app.name}</span>
                  </span>
                  {agents.map((a) => {
                    const on = agentAllowed(app, a.id);
                    return (
                      <span key={a.id} className="flex justify-center">
                        <button
                          type="button"
                          aria-label={`${on ? "Block" : "Allow"} ${app.name} for ${a.name}`}
                          onClick={() => void toggleAgent(app.id, a.id, !on)}
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
      {addOpen && <AddAppModal onClose={() => setAddOpen(false)} />}
    </div>
  );
}
