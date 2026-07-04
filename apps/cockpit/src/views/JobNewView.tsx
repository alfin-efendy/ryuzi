import { useEffect, useState } from "react";
import { ChevronDown, Folder, GitBranch, Server } from "lucide-react";
import { useRuntimes } from "@/store-runtimes";
import { useScheduler } from "@/store-scheduler";
import { useGateways } from "@/store-gateways";
import { useNav } from "@/store-nav";
import { useStore } from "@/store";
import { Card, CardHeader, CardRow, CardTitle } from "@/components/common/Card";
import { BackButton } from "@/components/common/DetailHeader";
import { MenuItem, MenuPanel } from "@/components/common/MenuPanel";
import { Switch } from "@/components/common/Switch";
import { StatusDot } from "@/components/common/bits";
import { ScheduleCard, type ScheduleValue } from "./JobDetailView";

export function JobNewView() {
  const { createJob } = useScheduler();
  const nav = useNav();
  const projects = useStore((s) => s.projects);
  const runtimes = useRuntimes((s) => s.runtimes);
  const { gateways, loaded: gwLoaded, hydrate: hydrateGw } = useGateways();

  const [prompt, setPrompt] = useState("");
  const [agentId, setAgentId] = useState("claude");
  const [projectId, setProjectId] = useState<string | null>(null);
  const [gateway, setGateway] = useState("local");
  const [schedule, setSchedule] = useState<ScheduleValue>({ mode: "natural", natural: "", cron: "0 9 * * *" });
  const [notifySuccess, setNotifySuccess] = useState(false);
  const [notifyFail, setNotifyFail] = useState(true);
  const [menu, setMenu] = useState<"agent" | "project" | "ws" | null>(null);
  const [saving, setSaving] = useState(false);

  useEffect(() => {
    if (!gwLoaded) void hydrateGw();
  }, [gwLoaded, hydrateGw]);

  const runnableAgents = runtimes.filter((a) => a.enabled && a.binaryPath && a.runnable);
  const agent = runtimes.find((a) => a.id === agentId) ?? runnableAgents[0];
  const project = projects.find((p) => p.projectId === projectId) ?? projects[0];
  const wsName = gateways.find((w) => w.id === gateway)?.name ?? gateway;
  const canCreate = prompt.trim().length > 0 && project !== undefined && !saving;
  const goScheduler = () => nav.navigate({ kind: "scheduler" });

  const create = async () => {
    if (!canCreate || !project) return;
    setSaving(true);
    const ok = await createJob({
      name: prompt.trim().slice(0, 40),
      mode: schedule.mode,
      natural: schedule.natural,
      cron: schedule.cron,
      projectId: project.projectId,
      branch: "main",
      agent: agent?.id ?? "claude",
      gateway,
      prompt: prompt.trim(),
      notifySuccess,
      notifyFail,
    });
    setSaving(false);
    if (ok) goScheduler();
  };

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[760px]">
        <BackButton label="Scheduler" onClick={goScheduler} />

        <h2 className="m-0 mb-1 text-[22px] font-semibold tracking-[-0.02em]">New job</h2>
        <p className="m-0 mb-5 text-[13px] text-muted-foreground">
          The prompt runs on a fresh session at every scheduled time, on the selected gateway.
        </p>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Prompt &amp; target</CardTitle>
          </CardHeader>
          <div className="px-[18px] pb-1 pt-3">
            <textarea
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="What should the agent do on every run?"
              rows={3}
              className="box-border w-full resize-y rounded-md border border-input bg-background px-3 py-2.5 font-sans text-[13px] leading-[1.55] text-foreground"
            />
          </div>
          <div className="relative flex flex-wrap items-center gap-1.5 px-[18px] pb-3.5 pt-2">
            <button
              type="button"
              onClick={() => setMenu(menu === "agent" ? null : "agent")}
              className="flex h-7 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-2.5 font-sans text-xs font-semibold text-foreground hover:bg-accent"
            >
              <StatusDot color={agent?.color ?? "var(--muted-foreground)"} size={7} />
              {agent?.name ?? "No agent"}
              <ChevronDown aria-hidden size={11} strokeWidth={2} />
            </button>
            <button
              type="button"
              onClick={() => setMenu(menu === "project" ? null : "project")}
              className="flex h-7 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-2.5 font-sans text-xs font-medium text-foreground hover:bg-accent"
            >
              <Folder aria-hidden size={12} strokeWidth={2} className="shrink-0" />
              {project?.name ?? "No project"}
              <ChevronDown aria-hidden size={11} strokeWidth={2} />
            </button>
            <span className="flex h-7 items-center gap-[7px] rounded-md border border-border px-2.5 font-mono text-[11.5px] text-muted-foreground">
              <GitBranch aria-hidden size={12} strokeWidth={2} className="shrink-0" />
              main
            </span>
            <button
              type="button"
              onClick={() => setMenu(menu === "ws" ? null : "ws")}
              className="flex h-7 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-2.5 font-sans text-xs font-medium text-foreground hover:bg-accent"
            >
              <Server aria-hidden size={12} strokeWidth={2} className="shrink-0" />
              {wsName}
              <ChevronDown aria-hidden size={11} strokeWidth={2} />
            </button>
            {menu === "agent" && (
              <MenuPanel onClose={() => setMenu(null)} className="bottom-11 left-[18px] w-[280px]">
                {runnableAgents.length === 0 && (
                  <div className="px-2.5 py-2 text-[12.5px] text-muted-foreground">No runnable agents detected.</div>
                )}
                {runnableAgents.map((a) => (
                  <MenuItem
                    key={a.id}
                    selected={a.id === agentId}
                    onClick={() => {
                      setAgentId(a.id);
                      setMenu(null);
                    }}
                  >
                    <StatusDot color={a.color} size={9} />
                    <span className="min-w-0 flex-1">
                      <span className="block text-[13px] font-medium">{a.name}</span>
                      <span className="block text-[11.5px] text-muted-foreground">{a.model || a.connection}</span>
                    </span>
                  </MenuItem>
                ))}
              </MenuPanel>
            )}
            {menu === "project" && (
              <MenuPanel onClose={() => setMenu(null)} className="bottom-11 left-[140px] w-[220px]">
                {projects.length === 0 && <div className="px-2.5 py-2 text-[12.5px] text-muted-foreground">No projects yet.</div>}
                {projects.map((p) => (
                  <MenuItem
                    key={p.projectId}
                    selected={p.projectId === project?.projectId}
                    onClick={() => {
                      setProjectId(p.projectId);
                      setMenu(null);
                    }}
                  >
                    <span className="flex-1 font-medium">{p.name}</span>
                  </MenuItem>
                ))}
              </MenuPanel>
            )}
            {menu === "ws" && (
              <MenuPanel onClose={() => setMenu(null)} className="bottom-11 left-[300px] w-[280px]">
                {gateways.map((w) => {
                  const eligible = w.id === "local";
                  return (
                    <MenuItem
                      key={w.id}
                      selected={w.id === gateway}
                      onClick={() => {
                        if (!eligible) return;
                        setGateway(w.id);
                        setMenu(null);
                      }}
                    >
                      <span className="flex h-[26px] w-[26px] shrink-0 items-center justify-center rounded-md bg-muted">
                        <span className="font-mono text-[9px] font-semibold text-muted-foreground">{w.badge}</span>
                      </span>
                      <span className={`min-w-0 flex-1 ${eligible ? "" : "opacity-50"}`}>
                        <span className="block text-[13px] font-medium">{w.name}</span>
                        <span className="block text-[11px] text-muted-foreground">
                          {eligible ? w.detail : "Runs require the remote daemon (coming)"}
                        </span>
                      </span>
                    </MenuItem>
                  );
                })}
              </MenuPanel>
            )}
          </div>
        </Card>

        <ScheduleCard
          value={schedule}
          next="—"
          nextWord="first run"
          onPatch={(p) => setSchedule((s) => ({ ...s, ...p }))}
          className="mb-3"
        />

        <Card className="mb-[18px]">
          <CardHeader>
            <CardTitle>Notifications</CardTitle>
          </CardHeader>
          <CardRow>
            <span className="flex-1 text-[13px] font-medium">On success</span>
            <Switch on={notifySuccess} onToggle={() => setNotifySuccess((v) => !v)} label="Notify on success" />
          </CardRow>
          <CardRow>
            <span className="flex-1 text-[13px] font-medium">On failure</span>
            <Switch on={notifyFail} onToggle={() => setNotifyFail((v) => !v)} label="Notify on failure" />
          </CardRow>
        </Card>

        <div className="flex items-center justify-end gap-2">
          <button
            type="button"
            onClick={goScheduler}
            className="h-8 cursor-pointer rounded-md border border-border bg-transparent px-3.5 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() => void create()}
            className={`h-8 rounded-md border-none bg-primary px-4 font-sans text-[12.5px] font-semibold text-primary-foreground ${
              canCreate ? "cursor-pointer hover:opacity-85" : "cursor-default opacity-45"
            }`}
          >
            {saving ? "Creating…" : "Create job"}
          </button>
        </div>
      </div>
    </div>
  );
}
