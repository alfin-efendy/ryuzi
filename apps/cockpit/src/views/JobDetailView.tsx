import { useEffect, useState } from "react";
import { ArrowUpRight, Check, ChevronDown, CircleAlert, Clock, Folder, GitBranch, Play, Server, Trash2 } from "lucide-react";
import { commands } from "@/bindings";
import { agentById, useAgents } from "@/store-agents";
import { formatDuration, formatNextRun, formatStarted, jobById, toInput, useScheduler } from "@/store-scheduler";
import { useGateways } from "@/store-gateways";
import { useNav } from "@/store-nav";
import { useStore } from "@/store";
import { Card, CardHeader, CardRow, CardTitle } from "@/components/common/Card";
import { BackButton, DetailHeader } from "@/components/common/DetailHeader";
import { MenuItem, MenuPanel } from "@/components/common/MenuPanel";
import { Segmented } from "@/components/common/Segmented";
import { Switch } from "@/components/common/Switch";
import { DiffStat, Pill, StatusDot } from "@/components/common/bits";

// ---- Schedule editor (shared with JobNewView) -------------------------------

export type ScheduleValue = { mode: string; natural: string; cron: string };

type Freq = "daily" | "weekly" | "hourly";

const MODE_OPTIONS: { id: string; label: string }[] = [
  { id: "natural", label: "Natural language" },
  { id: "visual", label: "Visual" },
  { id: "cron", label: "Cron" },
];

const FREQ_OPTIONS: { id: Freq; label: string }[] = [
  { id: "daily", label: "Daily" },
  { id: "weekly", label: "Weekly" },
  { id: "hourly", label: "Hourly" },
];

const DAY_OPTIONS: { n: number; label: string }[] = [
  { n: 1, label: "Mon" },
  { n: 2, label: "Tue" },
  { n: 3, label: "Wed" },
  { n: 4, label: "Thu" },
  { n: 5, label: "Fri" },
  { n: 6, label: "Sat" },
  { n: 0, label: "Sun" },
];

function composeCron(freq: Freq, days: number[], time: string): string {
  const [h = "9", m = "0"] = time.split(":");
  if (freq === "hourly") return "0 * * * *";
  if (freq === "daily") return `${Number(m)} ${Number(h)} * * *`;
  const list = DAY_OPTIONS.filter((d) => days.includes(d.n)).map((d) => d.n);
  return `${Number(m)} ${Number(h)} * * ${list.length > 0 ? list.join(",") : "*"}`;
}

function visualHuman(freq: Freq, days: number[], time: string): string {
  if (freq === "hourly") return "Every hour";
  if (freq === "daily") return `Every day at ${time}`;
  const names = DAY_OPTIONS.filter((d) => days.includes(d.n)).map((d) => d.label);
  return `Every ${names.length > 0 ? names.join(", ") : "week"} at ${time}`;
}

export function ScheduleCard({
  value,
  next,
  nextWord,
  onPatch,
  className,
}: {
  value: ScheduleValue;
  next: string;
  nextWord: string;
  onPatch: (patch: Partial<ScheduleValue>) => void;
  className?: string;
}) {
  // Visual-builder selections live locally; only the composed cron is persisted.
  const [freq, setFreq] = useState<Freq>("daily");
  const [days, setDays] = useState<number[]>([1]);
  const [time, setTime] = useState("09:00");
  const [parseFail, setParseFail] = useState(false);

  // The engine's parser is the single source of truth for natural phrases.
  useEffect(() => {
    if (value.mode !== "natural") {
      setParseFail(false);
      return;
    }
    const text = value.natural.trim();
    if (!text) {
      setParseFail(false);
      return;
    }
    let cancelled = false;
    void commands.parseNaturalSchedule(text).then((cron) => {
      if (cancelled) return;
      setParseFail(cron === null);
      if (cron !== null && cron !== value.cron) onPatch({ cron });
    });
    return () => {
      cancelled = true;
    };
  }, [value.mode, value.natural, value.cron, onPatch]);

  const human =
    value.mode === "cron" ? value.cron : value.mode === "natural" ? value.natural.trim() || value.cron : visualHuman(freq, days, time);

  const setVisual = (f: Freq, d: number[], t: string) => {
    setFreq(f);
    setDays(d);
    setTime(t);
    onPatch({ cron: composeCron(f, d, t) });
  };

  return (
    <Card className={className}>
      <CardHeader>
        <CardTitle>Schedule</CardTitle>
        <span className="flex-1" />
        <Segmented
          options={MODE_OPTIONS}
          value={value.mode}
          onChange={(m) => onPatch(m === "visual" ? { mode: m, cron: composeCron(freq, days, time) } : { mode: m })}
        />
      </CardHeader>
      <div className="flex flex-col gap-2.5 px-[18px] py-3.5">
        {value.mode === "natural" && (
          <>
            <input
              value={value.natural}
              onChange={(e) => onPatch({ natural: e.target.value })}
              placeholder="e.g. “every Monday at 9am” or “every 6 hours”"
              className="box-border h-9 w-full rounded-md border border-input bg-background px-3 font-sans text-[13px] text-foreground"
            />
            {parseFail && (
              <div className="flex items-center gap-[7px] text-xs" style={{ color: "#F59E0B" }}>
                <CircleAlert aria-hidden size={12} strokeWidth={2} className="shrink-0" />
                Couldn&#8217;t parse that — try &#8220;every day at 2am&#8221;, &#8220;every monday at 9am&#8221;, &#8220;every 6
                hours&#8221;, or switch to cron mode.
              </div>
            )}
          </>
        )}
        {value.mode === "visual" && (
          <div className="flex flex-wrap items-center gap-3">
            <Segmented options={FREQ_OPTIONS} value={freq} onChange={(f) => setVisual(f, days, time)} />
            {freq === "weekly" && (
              <div className="flex gap-1">
                {DAY_OPTIONS.map((d) => {
                  const sel = days.includes(d.n);
                  return (
                    <button
                      key={d.n}
                      type="button"
                      onClick={() => setVisual(freq, sel ? days.filter((x) => x !== d.n) : [...days, d.n], time)}
                      className={`h-[26px] cursor-pointer rounded-full border px-[9px] font-sans text-[11.5px] font-medium ${
                        sel
                          ? "border-primary bg-primary text-primary-foreground"
                          : "border-border bg-transparent text-muted-foreground hover:bg-accent hover:text-accent-foreground"
                      }`}
                    >
                      {d.label}
                    </button>
                  );
                })}
              </div>
            )}
            {freq !== "hourly" && (
              <input
                type="time"
                value={time}
                onChange={(e) => setVisual(freq, days, e.target.value)}
                className="h-[30px] rounded-md border border-input bg-background px-2.5 font-mono text-[12.5px] text-foreground"
              />
            )}
          </div>
        )}
        {value.mode === "cron" && (
          <input
            value={value.cron}
            onChange={(e) => onPatch({ cron: e.target.value })}
            className="box-border h-9 w-[200px] rounded-md border border-input bg-background px-3 font-mono text-[13px] text-foreground"
          />
        )}
        <div className="flex items-center gap-[7px] text-xs text-muted-foreground">
          <Check aria-hidden size={12} strokeWidth={2.5} className="shrink-0" style={{ color: "#22C55E" }} />
          <span>
            {human} · <span className="font-mono">{value.cron}</span> · {nextWord} {next}
          </span>
        </div>
      </div>
    </Card>
  );
}

// ---- Job detail -------------------------------------------------------------

const RUN_META: Record<string, { color: string; label: string }> = {
  success: { color: "#22C55E", label: "Success" },
  failed: { color: "#EF4444", label: "Failed" },
  running: { color: "#3B82F6", label: "Running" },
};

export function JobDetailView({ id }: { id: string }) {
  const { jobs, loaded, hydrate, toggle, updateJob, remove, runNow } = useScheduler();
  const agents = useAgents((s) => s.agents);
  const gateways = useGateways((s) => s.gateways);
  const setFocused = useStore((s) => s.setFocused);
  const nav = useNav();
  const [agentMenuOpen, setAgentMenuOpen] = useState(false);
  const [promptDraft, setPromptDraft] = useState<string | null>(null);

  useEffect(() => {
    if (!loaded) void hydrate();
  }, [loaded, hydrate]);

  const j = jobById(jobs, id);
  if (!j) {
    return <div className="flex flex-1 items-center justify-center text-[13px] text-muted-foreground">Job not found.</div>;
  }

  const agent = agentById(agents, j.agent);
  const ws = gateways.find((w) => w.id === j.gateway);
  const wsName = ws?.name ?? j.gateway;
  const failedRuns = j.history.filter((r) => r.status === "failed").length;
  const patch = (p: Partial<ReturnType<typeof toInput>>) => void updateJob(j.id, { ...toInput(j), ...p });

  return (
    <div className="min-h-0 flex-1 overflow-y-auto px-8 pb-10 pt-[22px]">
      <div className="mx-auto max-w-[760px]">
        <BackButton label="Scheduler" onClick={() => nav.navigate({ kind: "scheduler" })} />

        <DetailHeader
          chip={
            <span className="flex h-11 w-11 shrink-0 items-center justify-center rounded-lg bg-muted text-muted-foreground">
              <Clock aria-hidden size={20} strokeWidth={2} />
            </span>
          }
          title={j.name}
          titleExtra={
            <Pill variant="mono" className="shrink-0">
              {j.cron}
            </Pill>
          }
          sub={`${j.natural.trim() || j.cron} · next run ${j.enabled ? formatNextRun(j.nextRunMs) : "paused"}`}
        >
          <button
            type="button"
            onClick={() => void runNow(j.id)}
            className="flex h-8 shrink-0 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-3 font-sans text-[12.5px] font-medium text-foreground hover:bg-accent"
          >
            <Play aria-hidden size={13} strokeWidth={2} />
            Run now
          </button>
          <button
            type="button"
            title="Delete job"
            onClick={() => {
              void remove(j.id);
              nav.navigate({ kind: "scheduler" });
            }}
            className="flex h-8 w-8 shrink-0 cursor-pointer items-center justify-center rounded-md border border-border bg-transparent text-destructive hover:bg-accent"
          >
            <Trash2 aria-hidden size={13} strokeWidth={2} />
          </button>
          <Switch on={j.enabled} onToggle={() => void toggle(j.id, !j.enabled)} label="Enabled" />
        </DetailHeader>

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Prompt &amp; target</CardTitle>
          </CardHeader>
          <div className="px-[18px] pb-1 pt-3">
            <textarea
              value={promptDraft ?? j.prompt}
              onChange={(e) => setPromptDraft(e.target.value)}
              onBlur={() => {
                if (promptDraft !== null && promptDraft !== j.prompt) patch({ prompt: promptDraft });
                setPromptDraft(null);
              }}
              rows={3}
              className="box-border w-full resize-y rounded-md border border-input bg-background px-3 py-2.5 font-sans text-[13px] leading-[1.55] text-foreground"
            />
          </div>
          <div className="relative flex flex-wrap items-center gap-1.5 px-[18px] pb-3.5 pt-2">
            <button
              type="button"
              onClick={() => setAgentMenuOpen((v) => !v)}
              className="flex h-7 cursor-pointer items-center gap-[7px] rounded-md border border-border bg-transparent px-2.5 font-sans text-xs font-semibold text-foreground hover:bg-accent"
            >
              <StatusDot color={agent?.color ?? "var(--muted-foreground)"} size={7} />
              {agent?.name ?? j.agent}
              <ChevronDown aria-hidden size={11} strokeWidth={2} />
            </button>
            <span className="flex h-7 items-center gap-[7px] rounded-md border border-border px-2.5 text-xs font-medium text-muted-foreground">
              <Folder aria-hidden size={12} strokeWidth={2} className="shrink-0" />
              {j.projectName}
            </span>
            <span className="flex h-7 items-center gap-[7px] rounded-md border border-border px-2.5 font-mono text-[11.5px] text-muted-foreground">
              <GitBranch aria-hidden size={12} strokeWidth={2} className="shrink-0" />
              {j.branch}
            </span>
            <span className="flex h-7 items-center gap-[7px] rounded-md border border-border px-2.5 text-xs font-medium text-muted-foreground">
              <Server aria-hidden size={12} strokeWidth={2} className="shrink-0" />
              {wsName}
            </span>
            {agentMenuOpen && (
              <MenuPanel onClose={() => setAgentMenuOpen(false)} className="bottom-11 left-[18px] w-[280px]">
                {agents
                  .filter((a) => a.enabled && a.binaryPath && a.runnable)
                  .map((a) => (
                    <MenuItem
                      key={a.id}
                      selected={a.id === j.agent}
                      onClick={() => {
                        patch({ agent: a.id });
                        setAgentMenuOpen(false);
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
          </div>
        </Card>

        <ScheduleCard
          value={{ mode: j.mode, natural: j.natural, cron: j.cron }}
          next={j.enabled ? formatNextRun(j.nextRunMs) : "paused"}
          nextWord="next run"
          onPatch={(p) => patch(p)}
          className="mb-3"
        />

        <Card className="mb-3">
          <CardHeader>
            <CardTitle>Notifications</CardTitle>
          </CardHeader>
          <CardRow>
            <div className="min-w-0 flex-1">
              <div className="text-[13px] font-medium">On success</div>
              <div className="mt-px text-[11.5px] text-muted-foreground">Toast with the run summary and diff stats.</div>
            </div>
            <Switch on={j.notifySuccess} onToggle={() => patch({ notifySuccess: !j.notifySuccess })} label="Notify on success" />
          </CardRow>
          <CardRow>
            <div className="min-w-0 flex-1">
              <div className="text-[13px] font-medium">On failure</div>
              <div className="mt-px text-[11.5px] text-muted-foreground">Notify immediately with the error.</div>
            </div>
            <Switch on={j.notifyFail} onToggle={() => patch({ notifyFail: !j.notifyFail })} label="Notify on failure" />
          </CardRow>
        </Card>

        <Card>
          <CardHeader>
            <CardTitle>Run history</CardTitle>
            <span className="text-xs text-muted-foreground">
              {j.history.length} runs · {failedRuns} failed
            </span>
          </CardHeader>
          {j.history.length === 0 && (
            <div className="px-[18px] py-4 text-[12.5px] text-muted-foreground">
              No runs yet. The first run happens at the next scheduled time, or trigger one with Run now.
            </div>
          )}
          {j.history.map((r) => {
            const meta = RUN_META[r.status] ?? RUN_META.failed;
            return (
              <div key={r.id} className="border-b border-border last:border-b-0">
                <div className="flex items-center gap-3 px-[18px] py-[11px]">
                  <StatusDot color={meta.color} size={8} pulse={r.status === "running"} />
                  <span className="w-16 shrink-0 text-xs font-semibold" style={{ color: meta.color }}>
                    {meta.label}
                  </span>
                  <span className="min-w-0 flex-1 text-[12.5px]">
                    {formatStarted(r.startedAtMs)}
                    {r.note && <span className="text-muted-foreground"> — {r.note}</span>}
                    {r.error && (
                      <span
                        className="mt-0.5 block overflow-hidden text-ellipsis whitespace-nowrap font-mono text-[11px]"
                        style={{ color: "#EF4444" }}
                      >
                        {r.error}
                      </span>
                    )}
                  </span>
                  <span className="shrink-0 font-mono text-[11.5px] text-muted-foreground">{formatDuration(r.durationMs)}</span>
                  {r.addLines !== null && r.delLines !== null && (
                    <DiffStat add={Number(r.addLines)} del={Number(r.delLines)} className="shrink-0 text-[11.5px]" />
                  )}
                  {r.sessionPk && (
                    <button
                      type="button"
                      onClick={() => {
                        if (!r.sessionPk) return;
                        setFocused(r.sessionPk);
                        nav.navigate({ kind: "session" });
                      }}
                      className="flex h-[26px] shrink-0 cursor-pointer items-center gap-1.5 rounded-md border border-border bg-transparent px-2.5 font-sans text-[11.5px] font-medium text-foreground hover:bg-accent"
                    >
                      Open session
                      <ArrowUpRight aria-hidden size={11} strokeWidth={2} />
                    </button>
                  )}
                </div>
              </div>
            );
          })}
        </Card>
      </div>
    </div>
  );
}
