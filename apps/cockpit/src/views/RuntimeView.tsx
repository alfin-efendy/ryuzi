import { Button, cn, SettingsCard as Card, Switch } from "@ryuzi/ui";
import { ChevronRight, Loader2, RefreshCw } from "lucide-react";
import { useEffect, useState } from "react";
import { commands } from "@/bindings";
import { Chip, Pill, StatusDot } from "@/components/common/bits";
import { useRuntimes } from "@/store-runtimes";
import { useNav } from "@/store-nav";

const SUCCESS = "#22C55E";

// Runtime settings list: every CLI agent Cockpit can drive, with real binary
// detection, default/update badges, enable toggles, and a detail screen path.
export function RuntimeView() {
  const { runtimes, refreshing, refresh, update, setDefault, updating, beginUpdate } = useRuntimes();
  const navigate = useNav((s) => s.navigate);
  const [configured, setConfigured] = useState<Record<string, boolean>>({});

  useEffect(() => {
    if (runtimes.length === 0) return;
    void Promise.all(runtimes.map((r) => commands.runtimeConfigStatus(r.id))).then((results) => {
      const next: Record<string, boolean> = {};
      results.forEach((res, i) => {
        if (res.status === "ok") next[runtimes[i].id] = res.data.configured;
      });
      setConfigured(next);
    });
  }, [runtimes]);

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 py-7">
      <div className="mx-auto max-w-[720px]">
        <div className="flex items-start gap-3">
          <div className="min-w-0 flex-1">
            <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">Runtime</h2>
            <p className="m-0 mb-5 text-[13px] text-muted-foreground">
              Cockpit is agent-agnostic. Any CLI coding agent can run a session — mix them across projects.
            </p>
          </div>
          <Button variant="outline" onClick={() => void refresh()} disabled={refreshing}>
            <RefreshCw aria-hidden size={13} strokeWidth={2} className={cn("size-[13px]", refreshing && "animate-spin")} />
            {refreshing ? "Detecting…" : "Re-detect"}
          </Button>
        </div>
        <div className="flex flex-col gap-3">
          {runtimes.map((agent) => {
            const installed = agent.binaryPath !== null;
            const isDefault = agent.isDefault;
            const hasUpdate =
              installed &&
              agent.latestVersion !== null &&
              agent.installedVersion !== null &&
              agent.latestVersion !== agent.installedVersion;
            const statusColor = installed ? SUCCESS : "var(--muted-foreground)";
            const open = () => navigate({ kind: "runtimeDetail", id: agent.id });
            const isUpdating = updating[agent.id] === true;
            return (
              <Card key={agent.id} className={cn("flex items-center gap-3.5 px-[18px] py-4", isDefault && "border-ring")}>
                <Chip initial={agent.initial} color={agent.color} size={36} onClick={open} />
                <Button
                  variant="ghost"
                  onClick={open}
                  className="h-auto min-w-0 flex-1 flex-col items-start gap-0 whitespace-normal p-0 text-left"
                >
                  <span className="flex items-center gap-2">
                    <span className="text-sm font-semibold text-foreground">{agent.name}</span>
                    {isDefault && <Pill variant="primary">Default</Pill>}
                    {configured[agent.id] && <Pill variant="mono">Routed via Ryuzi</Pill>}
                  </span>
                  <span className="mt-0.5 block text-xs font-normal text-muted-foreground">
                    {installed ? `${agent.model || agent.connection} · ${agent.connection}` : "Not installed"}
                  </span>
                </Button>
                {hasUpdate && (
                  <Button
                    variant="ghost"
                    size="xs"
                    disabled={isUpdating}
                    onClick={() => void beginUpdate(agent.id)}
                    title={agent.npmPackage ? `npm install -g ${agent.npmPackage}@latest` : undefined}
                    className="rounded-full text-[10.5px] font-semibold tracking-[0.02em]"
                    style={{ background: "color-mix(in oklab, #F59E0B 18%, transparent)", color: "#F59E0B" }}
                  >
                    {isUpdating && <Loader2 aria-hidden size={10} strokeWidth={2} className="size-2.5 animate-spin" />}
                    {isUpdating ? "Updating…" : `Update ${agent.latestVersion}`}
                  </Button>
                )}
                <span className="flex shrink-0 items-center gap-1.5 text-xs" style={{ color: statusColor }}>
                  <StatusDot color={statusColor} />
                  {installed ? (agent.installedVersion ? `v${agent.installedVersion}` : "Installed") : "Not found"}
                </span>
                {!isDefault && agent.enabled && installed && (
                  <Button variant="outline" size="sm" onClick={() => void setDefault(agent.id)}>
                    Make default
                  </Button>
                )}
                <Switch
                  on={agent.enabled && installed}
                  onToggle={() => installed && void update(agent.id, { enabled: !agent.enabled })}
                  label={`${agent.name} enabled`}
                />
                <Button variant="ghost" size="icon-sm" onClick={open} title="Details" className="text-muted-foreground">
                  <ChevronRight aria-hidden size={14} strokeWidth={2} className="size-3.5" />
                </Button>
              </Card>
            );
          })}
        </div>
      </div>
    </div>
  );
}
