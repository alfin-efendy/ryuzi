import { CircleAlert } from "lucide-react";
import { Card, CardHeader, CardHint, CardRow, CardTitle } from "@/components/common/Card";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { Segmented } from "@/components/common/Segmented";
import { Switch } from "@/components/common/Switch";
import { Chip, StatusDot } from "@/components/common/bits";
import { AGENT_IDS, AGENTS, WORKSPACES } from "@/fixtures";
import { useFixtures } from "@/store-fixtures";
import { useNav } from "@/store-nav";

const smallOutlineBtn =
  "h-[27px] shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-[11px] font-sans text-xs font-medium text-foreground hover:bg-accent";
const rowLabel = "w-[120px] shrink-0 text-[13px] font-medium";

export function AppDetailView({ id }: { id: string }) {
  const nav = useNav();
  const { apps, setAppScope, toggleAppWs, setToolPerm, toggleAppAgent, uninstallApp } = useFixtures();
  const app = apps.find((a) => a.id === id);
  const goApps = () => nav.navigate({ kind: "apps" });

  // Uninstalled (or unknown) apps have no detail to show — the uninstall action
  // navigates back explicitly, so this only covers stale/deep-linked ids.
  if (!app) return null;

  const auth = app.auth;
  const status = app.status === "error" ? { color: "#EF4444", label: "Error" } : { color: "#22C55E", label: "Connected" };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[720px]">
        <BackButton label="Apps" onClick={goApps} />

        <DetailHeader
          chip={<Chip initial={app.initial} color={app.color} size={44} mono />}
          title={app.name}
          sub={`${app.kind} · v${app.version} · ${app.publisher}`}
        >
          <span className="flex shrink-0 items-center gap-1.5 text-xs" style={{ color: status.color }}>
            <StatusDot color={status.color} />
            {status.label}
          </span>
          <button
            type="button"
            onClick={() => {
              uninstallApp(app.id);
              goApps();
            }}
            className="h-8 shrink-0 cursor-pointer rounded-md border border-border bg-transparent px-3 font-sans text-[12.5px] font-medium text-destructive hover:bg-accent"
          >
            Uninstall
          </button>
        </DetailHeader>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Connection</CardTitle>
          </CardHeader>
          {auth.type === "oauth" && (
            <>
              {auth.status === "expired" && (
                <CardRow>
                  <CircleAlert aria-hidden size={16} strokeWidth={2} className="shrink-0" style={{ color: "#F59E0B" }} />
                  <span className="flex-1 text-[12.5px]">
                    The OAuth token expired on {auth.expires}. Agents can’t call this app until you sign in again.
                  </span>
                  <button
                    type="button"
                    className="h-[30px] shrink-0 cursor-pointer rounded-md border-none bg-primary px-3.5 font-sans text-[12.5px] font-medium text-primary-foreground hover:opacity-85"
                  >
                    Re-authenticate
                  </button>
                </CardRow>
              )}
              <CardRow>
                <span className={rowLabel}>Account</span>
                <span className="flex-1 font-mono text-xs text-muted-foreground">{auth.account}</span>
              </CardRow>
              <CardRow>
                <span className={rowLabel}>Token expires</span>
                <span className="flex-1 text-[12.5px] text-muted-foreground">{auth.expires}</span>
              </CardRow>
              <CardRow>
                <span className={rowLabel}>Last refreshed</span>
                <span className="flex-1 text-[12.5px] text-muted-foreground">{auth.lastRefresh}</span>
                {auth.status === "connected" && (
                  <button type="button" className={smallOutlineBtn}>
                    Refresh now
                  </button>
                )}
              </CardRow>
            </>
          )}
          {auth.type === "env" && (
            <>
              <CardRow>
                <span className={rowLabel}>Environment</span>
                <span className="flex-1 font-mono text-xs text-muted-foreground">{auth.env}</span>
                <button type="button" className={smallOutlineBtn}>
                  Edit
                </button>
              </CardRow>
              <CardRow>
                <span className={rowLabel}>Connects as</span>
                <span className="flex-1 font-mono text-xs text-muted-foreground">{auth.account}</span>
              </CardRow>
            </>
          )}
          {auth.type === "none" && (
            <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">
              No authentication required — runs locally on the gateway.
            </div>
          )}
        </Card>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Scope</CardTitle>
            <CardHint>Where this app is installed</CardHint>
            <span className="flex-1" />
            <Segmented
              options={[
                { id: "global", label: "Global" },
                { id: "select", label: "Select" },
              ]}
              value={app.scope}
              onChange={(scope) => setAppScope(app.id, scope)}
            />
          </CardHeader>
          {app.scope === "select" && (
            <div className="flex flex-wrap gap-1.5 px-[18px] py-3">
              {WORKSPACES.map((w) => {
                const sel = !!app.scopeWs[w.id];
                return (
                  <button
                    key={w.id}
                    type="button"
                    onClick={() => toggleAppWs(app.id, w.id)}
                    className={`flex h-7 cursor-pointer items-center gap-[7px] rounded-full border px-[11px] font-sans text-xs font-medium ${
                      sel ? "border-transparent bg-primary text-primary-foreground" : "border-border bg-transparent text-muted-foreground"
                    }`}
                  >
                    <span className="font-mono text-[9.5px] font-semibold opacity-75">{w.badge}</span>
                    {w.name}
                  </button>
                );
              })}
            </div>
          )}
        </Card>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Tools</CardTitle>
            <CardHint>{app.tools.length} tools · per-tool permission for every agent</CardHint>
          </CardHeader>
          {app.tools.map((t) => (
            <div key={t.name} className="flex items-center gap-3 border-b border-border px-[18px] py-[11px] last:border-b-0">
              <div className="min-w-0 flex-1">
                <div className="font-mono text-[12.5px] font-semibold">{t.name}</div>
                <div className="mt-px text-[11.5px] text-muted-foreground">{t.desc}</div>
              </div>
              <Segmented
                size="sm"
                options={[
                  { id: "allow", label: "Allow" },
                  { id: "ask", label: "Ask" },
                  { id: "deny", label: "Deny" },
                ]}
                value={t.perm}
                onChange={(perm) => setToolPerm(app.id, t.name, perm)}
              />
            </div>
          ))}
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Agent access</CardTitle>
            <CardHint>Which agents may call this app</CardHint>
          </CardHeader>
          {AGENT_IDS.map((aid) => {
            const agent = AGENTS[aid];
            return (
              <div key={aid} className="flex items-center gap-3 border-b border-border px-[18px] py-[11px] last:border-b-0">
                <StatusDot color={agent.color} size={8} />
                <span className="min-w-0 flex-1">
                  <span className="block text-[13px] font-medium">{agent.name}</span>
                  <span className="block text-[11px] text-muted-foreground">{agent.model}</span>
                </span>
                <Switch on={!!app.agentAccess[aid]} onToggle={() => toggleAppAgent(app.id, aid)} label={`${agent.name} access`} />
              </div>
            );
          })}
        </Card>
      </div>
    </div>
  );
}
