import { RefreshCw } from "lucide-react";
import { useEffect } from "react";
import {
  Button,
  Segmented,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardHint as CardHint,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
  Switch,
} from "@ryuzi/ui";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { Chip, StatusDot } from "@/components/common/bits";
import { agentAllowed, appById, useApps } from "@/store-apps";
import { NATIVE_AGENT } from "@/constants";
import { useGateways } from "@/store-gateways";
import { useNav } from "@/store-nav";

const rowLabel = "w-[120px] shrink-0 text-[13px] font-medium";

export function AppDetailView({ id }: { id: string }) {
  const nav = useNav();
  const { apps, loaded, hydrate, probing, probe, remove, setScope, setToolPerm, toggleAgent } = useApps();
  const gateways = useGateways((s) => s.gateways);
  const goApps = () => nav.navigate({ kind: "plugins" });

  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

  const app = appById(apps, id);
  if (!app) return null;

  const status =
    app.status === "connected"
      ? { color: "#22C55E", label: "Connected" }
      : app.status === "error"
        ? { color: "#EF4444", label: "Error" }
        : { color: "var(--muted-foreground)", label: "Unchecked" };
  const isProbing = probing === app.id;

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[720px]">
        <BackButton label="Plugins" onClick={goApps} />

        <DetailHeader
          chip={<Chip initial={app.initial} color={app.color} size={44} mono />}
          title={app.name}
          sub={[app.kind, app.version ? `v${app.version}` : null, app.publisher].filter(Boolean).join(" · ")}
        >
          <span className="flex shrink-0 items-center gap-1.5 text-xs" style={{ color: status.color }}>
            <StatusDot color={status.color} />
            {status.label}
          </span>
          <Button variant="outline" onClick={() => void probe(app.id)} disabled={isProbing}>
            <RefreshCw aria-hidden size={13} strokeWidth={2} className={isProbing ? "size-[13px] animate-spin" : "size-[13px]"} />
            {isProbing ? "Connecting…" : "Reconnect"}
          </Button>
          <Button
            variant="destructive"
            onClick={() => {
              void remove(app.id);
              goApps();
            }}
          >
            Uninstall
          </Button>
        </DetailHeader>

        {app.status === "error" && app.statusDetail && (
          <Card className="mb-3 px-[18px] py-3 text-[12.5px]">
            <span style={{ color: "#EF4444" }}>{app.statusDetail}</span>
          </Card>
        )}

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Connection</CardTitle>
          </CardHeader>
          <CardRow>
            <span className={rowLabel}>{app.transport === "http" ? "URL" : "Command"}</span>
            <span className="flex-1 truncate font-mono text-xs text-muted-foreground">
              {app.transport === "http" ? (app.url ?? "—") : [app.command, ...app.args].filter(Boolean).join(" ")}
            </span>
          </CardRow>
          {app.authKind === "env" ? (
            <CardRow>
              <span className={rowLabel}>Environment</span>
              <span className="flex-1 font-mono text-xs text-muted-foreground">{app.authDetail ?? "—"}</span>
            </CardRow>
          ) : (
            <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">
              No authentication configured — runs with the environment it inherits.
            </div>
          )}
        </Card>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Scope</CardTitle>
            <CardHint>Where this plugin is attached</CardHint>
            <span className="flex-1" />
            <Segmented
              options={[
                { id: "global", label: "Global" },
                { id: "select", label: "Select" },
              ]}
              value={app.scope}
              onChange={(scope) => void setScope(app.id, scope, app.scopeGateways)}
            />
          </CardHeader>
          {app.scope === "select" && (
            <div className="flex flex-wrap gap-1.5 px-[18px] py-3">
              {gateways.map((w) => {
                const sel = app.scopeGateways.includes(w.id);
                return (
                  <Button
                    key={w.id}
                    variant={sel ? "default" : "outline"}
                    size="sm"
                    onClick={() =>
                      void setScope(app.id, app.scope, sel ? app.scopeGateways.filter((x) => x !== w.id) : [...app.scopeGateways, w.id])
                    }
                    className="rounded-full"
                  >
                    <span className="font-mono text-[9.5px] font-semibold opacity-75">{w.badge}</span>
                    {w.name}
                  </Button>
                );
              })}
            </div>
          )}
        </Card>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Tools</CardTitle>
            <CardHint>
              {app.tools.length > 0 ? `${app.tools.length} tools · per-tool permission for every agent` : "Discovered on connect"}
            </CardHint>
          </CardHeader>
          {app.tools.length === 0 && (
            <div className="px-[18px] py-3.5 text-[12.5px] text-muted-foreground">
              No tools discovered yet — reconnect to run the handshake.
            </div>
          )}
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
                onChange={(perm) => void setToolPerm(app.id, t.name, perm)}
              />
            </div>
          ))}
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Agent access</CardTitle>
            <CardHint>Whether the agent may call this plugin</CardHint>
          </CardHeader>
          <div className="flex items-center gap-3 px-[18px] py-[11px]">
            <StatusDot color={NATIVE_AGENT.color} size={8} />
            <span className="min-w-0 flex-1">
              <span className="block text-[13px] font-medium">Allow the agent to use this app</span>
              <span className="block text-[11px] text-muted-foreground">{NATIVE_AGENT.name} · applies to every session</span>
            </span>
            <Switch on={agentAllowed(app)} onToggle={() => void toggleAgent(app.id, !agentAllowed(app))} label="Agent access" />
          </div>
        </Card>
      </div>
    </div>
  );
}
