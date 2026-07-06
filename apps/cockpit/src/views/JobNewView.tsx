import { useEffect, useState } from "react";
import { ChevronDown, Folder, GitBranch, Server } from "lucide-react";
import { useScheduler } from "@/store-scheduler";
import { useGateways } from "@/store-gateways";
import { useNav } from "@/store-nav";
import { useStore } from "@/store";
import {
  Button,
  Combobox,
  SettingsCard as Card,
  SettingsCardHeader as CardHeader,
  SettingsCardRow as CardRow,
  SettingsCardTitle as CardTitle,
  Switch,
  Textarea,
} from "@ryuzi/ui";
import { toast } from "sonner";
import { commands, type BranchList } from "@/bindings";
import { BackButton } from "@/components/common/DetailHeader";
import { ScheduleCard, type ScheduleValue } from "./JobDetailView";

export function JobNewView() {
  const { createJob } = useScheduler();
  const nav = useNav();
  const projects = useStore((s) => s.projects);
  const { gateways, loaded: gwLoaded, hydrate: hydrateGw } = useGateways();

  const [prompt, setPrompt] = useState("");
  const [projectId, setProjectId] = useState<string | null>(null);
  const [gateway, setGateway] = useState("local");
  const [schedule, setSchedule] = useState<ScheduleValue>({ mode: "natural", natural: "", cron: "0 9 * * *" });
  const [notifySuccess, setNotifySuccess] = useState(false);
  const [notifyFail, setNotifyFail] = useState(true);
  const [saving, setSaving] = useState(false);
  const [branch, setBranch] = useState<string | null>(null);
  const [branchList, setBranchList] = useState<BranchList | null>(null);

  useEffect(() => {
    if (!gwLoaded) void hydrateGw();
  }, [gwLoaded, hydrateGw]);

  const project = projects.find((p) => p.projectId === projectId) ?? projects[0];
  const wsName = gateways.find((w) => w.id === gateway)?.name ?? gateway;

  const branchProjectId = project?.projectId ?? null;
  useEffect(() => {
    setBranchList(null);
    setBranch(null);
    if (!branchProjectId) return;
    let cancelled = false;
    void commands.listBranches(branchProjectId).then((res) => {
      if (cancelled) return;
      if (res.status === "ok") {
        setBranchList(res.data);
        // A detached HEAD's "current" is a short commit id, not a branch name —
        // preselecting it would let the user unknowingly create a branch named
        // after a commit id. Leave the selection null; the placeholder shows.
        if (!res.data.detached) setBranch(res.data.current);
      } else {
        toast.error("Couldn't list branches: " + res.error.message);
      }
    });
    return () => {
      cancelled = true;
    };
  }, [branchProjectId]);

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
      branch: branch ?? "main",
      // Ryuzi-only sessions: jobs always run the native runtime.
      agent: "native",
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
            <Textarea
              value={prompt}
              onChange={(e) => setPrompt(e.target.value)}
              placeholder="What should the agent do on every run?"
              rows={3}
              className="resize-y"
            />
          </div>
          <div className="relative flex flex-wrap items-center gap-1.5 px-[18px] pb-3.5 pt-2">
            <Combobox
              aria-label="Project"
              options={projects.map((p) => ({ value: p.projectId, label: p.name }))}
              value={project?.projectId ?? null}
              onValueChange={(id) => setProjectId(id)}
              placeholder="No project"
              trigger={
                <Button variant="outline" size="sm">
                  <Folder aria-hidden size={12} strokeWidth={2} className="size-3" />
                  {project?.name ?? "No project"}
                  <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
                </Button>
              }
            />
            <Combobox
              aria-label="Branch"
              options={(branchList?.branches ?? []).map((b) => ({ value: b, label: b, mono: true }))}
              value={branch}
              onValueChange={setBranch}
              placeholder="Branch"
              trigger={
                <Button variant="outline" size="sm" className="gap-[7px] font-mono text-[11.5px] text-muted-foreground">
                  <GitBranch aria-hidden size={12} strokeWidth={2} className="shrink-0" />
                  {branch ?? "main"}
                  <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
                </Button>
              }
            />
            <Combobox
              aria-label="Workspace"
              options={gateways.map((w) => ({
                value: w.id,
                label: w.name,
                description: w.id === "local" ? w.detail : "Runs require the remote daemon (coming)",
              }))}
              value={gateway}
              onValueChange={(id) => {
                if (id === "local") setGateway(id);
              }}
              trigger={
                <Button variant="outline" size="sm">
                  <Server aria-hidden size={12} strokeWidth={2} className="size-3" />
                  {wsName}
                  <ChevronDown aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
                </Button>
              }
            />
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
          <Button variant="outline" onClick={goScheduler}>
            Cancel
          </Button>
          <Button onClick={() => void create()} disabled={!canCreate}>
            {saving ? "Creating…" : "Create job"}
          </Button>
        </div>
      </div>
    </div>
  );
}
