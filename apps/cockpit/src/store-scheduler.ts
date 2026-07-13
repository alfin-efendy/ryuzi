import { create } from "zustand";
import { toast } from "sonner";
import { commands, events, type CmdError, type JobInfo, type JobInput, type Result } from "./bindings";

// Scheduler domain store. Jobs persist in the engine; the core runner loop
// fires them for real. Run history updates live off jobRunChanged events.

type SchedulerState = {
  jobs: JobInfo[];
  loaded: boolean;
  hydrate: () => Promise<void>;
  refresh: () => Promise<void>;
  createJob: (input: JobInput) => Promise<boolean>;
  updateJob: (id: string, input: JobInput) => Promise<boolean>;
  toggle: (id: string, enabled: boolean) => Promise<void>;
  remove: (id: string) => Promise<boolean>;
  runNow: (id: string) => Promise<void>;
};

function applyResult(set: (partial: Partial<SchedulerState>) => void, res: Result<JobInfo[], CmdError>, action: string): boolean {
  if (res.status === "ok") {
    set({ jobs: res.data, loaded: true });
    return true;
  }
  toast.error(`${action} failed: ${res.error.message}`);
  return false;
}

let listening = false;

export const useScheduler = create<SchedulerState>((set, get) => ({
  jobs: [],
  loaded: false,

  hydrate: async () => {
    applyResult(set, await commands.listJobs("local"), "Job list");
    if (!listening) {
      listening = true;
      void events.coreEventMsg.listen((e) => {
        if (e.payload.event.kind === "jobRunChanged") void get().refresh();
      });
    }
  },

  refresh: async () => {
    applyResult(set, await commands.listJobs("local"), "Job list");
  },

  createJob: async (input) => applyResult(set, await commands.createJob("local", input), "Create job"),

  updateJob: async (id, input) => applyResult(set, await commands.updateJob("local", id, input), "Update job"),

  toggle: async (id, enabled) => {
    set({ jobs: get().jobs.map((j) => (j.id === id ? { ...j, enabled } : j)) });
    applyResult(set, await commands.toggleJob("local", id, enabled), "Toggle job");
  },

  remove: async (id) => applyResult(set, await commands.deleteJob("local", id), "Delete job"),

  runNow: async (id) => {
    applyResult(set, await commands.runJobNow("local", id), "Run job");
  },
}));

export function jobById(jobs: JobInfo[], id: string): JobInfo | undefined {
  return jobs.find((j) => j.id === id);
}

/** Build the JobInput mirror of an existing job for partial updates. */
export function toInput(j: JobInfo): JobInput {
  return {
    name: j.name,
    mode: j.mode,
    natural: j.natural,
    cron: j.cron,
    projectId: j.projectId,
    branch: j.branch,
    gateway: j.gateway,
    prompt: j.prompt,
    notifySuccess: j.notifySuccess,
    notifyFail: j.notifyFail,
  };
}

/** "in 3h 12m" under a day, else "Mon 09:00". */
export function formatNextRun(ms: number | null): string {
  if (ms === null) return "—";
  const delta = ms - Date.now();
  if (delta <= 0) return "due now";
  const mins = Math.ceil(delta / 60_000);
  if (mins < 60) return `in ${mins}m`;
  if (mins < 24 * 60) return `in ${Math.floor(mins / 60)}h ${mins % 60}m`;
  const d = new Date(ms);
  return `${d.toLocaleDateString(undefined, { weekday: "short" })} ${d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", hour12: false })}`;
}

export function formatStarted(ms: number): string {
  const d = new Date(ms);
  const now = new Date();
  const time = d.toLocaleTimeString(undefined, { hour: "2-digit", minute: "2-digit", hour12: false });
  if (d.toDateString() === now.toDateString()) return `Today ${time}`;
  const yesterday = new Date(now);
  yesterday.setDate(now.getDate() - 1);
  if (d.toDateString() === yesterday.toDateString()) return `Yesterday ${time}`;
  return `${d.toLocaleDateString(undefined, { month: "short", day: "numeric" })}, ${time}`;
}

export function formatDuration(ms: number | null): string {
  if (ms === null) return "…";
  const secs = Math.max(0, Math.round(ms / 1000));
  return `${Math.floor(secs / 60)}m ${String(secs % 60).padStart(2, "0")}s`;
}
