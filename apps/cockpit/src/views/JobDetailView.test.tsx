import { afterEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { CmdError, GatewayInfo, JobInfo, Result, RunInfo } from "@/bindings";

// Mock the Tauri boundary before the view (and the stores it pulls in) load.
let seededJobs: JobInfo[] = [];
const ok = (jobs: JobInfo[]): Result<JobInfo[], CmdError> => ({ status: "ok", data: jobs });

const listJobs = mock(async () => ok(seededJobs));
const updateJob = mock(async () => ok(seededJobs));
const toggleJob = mock(async () => ok(seededJobs));
const deleteJob = mock(async () => ok([]));
const runJobNow = mock(async () => ok(seededJobs));
const parseNaturalSchedule = mock(async () => null);

mock.module("@/bindings", () => ({
  commands: { listJobs, updateJob, toggleJob, deleteJob, runJobNow, parseNaturalSchedule },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));

const { JobDetailView } = await import("./JobDetailView");
const { useScheduler } = await import("@/store-scheduler");
const { useGateways } = await import("@/store-gateways");

const gateway: GatewayInfo = {
  id: "local",
  name: "Local host",
  badge: "L",
  kind: "local",
  detail: "",
  metaLine: "",
  status: "connected",
  latency: null,
  daemonVersion: "0.4.0",
  uptime: null,
  lastSeenMs: null,
  resources: [],
  fingerprint: null,
  fsMode: "full",
  paths: [],
};

function makeJob(overrides: Partial<JobInfo> = {}): JobInfo {
  return {
    id: "job-1",
    name: "Nightly triage",
    cron: "0 2 * * *",
    mode: "cron",
    natural: "",
    projectId: "proj-1",
    projectName: "ryuzi",
    branch: "main",
    agent: "claude",
    gateway: "local",
    enabled: true,
    prompt: "Triage new issues and open a summary PR.",
    notifySuccess: true,
    notifyFail: false,
    nextRunMs: null,
    history: [],
    ...overrides,
  };
}

// Seeding loaded: true keeps the mount effect from re-hydrating over IPC.
function seed(jobs: JobInfo[]) {
  seededJobs = jobs;
  useScheduler.setState({ jobs, loaded: true });
  useGateways.setState({ gateways: [gateway] });
}

afterEach(() => {
  cleanup();
  runJobNow.mockClear();
  updateJob.mockClear();
  toggleJob.mockClear();
  deleteJob.mockClear();
});

test("renders the job identity, prompt, and target chips from store data", () => {
  seed([makeJob()]);
  render(<JobDetailView id="job-1" />);

  expect(screen.getByText("Nightly triage")).toBeTruthy();
  // Cron shows in the header pill and the schedule footer.
  expect(screen.getAllByText("0 2 * * *").length).toBeGreaterThanOrEqual(2);
  expect(screen.getByDisplayValue("Triage new issues and open a summary PR.")).toBeTruthy();
  // Ryuzi-only: no agent picker; the target chips are project/branch/gateway.
  expect(screen.queryByRole("button", { name: "Claude Code" })).toBeNull();
  expect(screen.getByText("ryuzi")).toBeTruthy();
  expect(screen.getByText("main")).toBeTruthy();
  expect(screen.getByText("Local host")).toBeTruthy();
});

test("renders section cards and reflects the enabled/notification switches", () => {
  seed([makeJob()]);
  render(<JobDetailView id="job-1" />);

  expect(screen.getByText("Prompt & target")).toBeTruthy();
  expect(screen.getByText("Schedule")).toBeTruthy();
  expect(screen.getByText("Notifications")).toBeTruthy();
  expect(screen.getByText("Run history")).toBeTruthy();
  expect(screen.getByRole("switch", { name: "Enabled" }).getAttribute("aria-checked")).toBe("true");
  expect(screen.getByRole("switch", { name: "Notify on success" }).getAttribute("aria-checked")).toBe("true");
  expect(screen.getByRole("switch", { name: "Notify on failure" }).getAttribute("aria-checked")).toBe("false");
});

test("shows the empty run-history state when the job has no runs", () => {
  seed([makeJob()]);
  render(<JobDetailView id="job-1" />);

  expect(screen.getByText("0 runs · 0 failed")).toBeTruthy();
  expect(screen.getByText(/No runs yet/)).toBeTruthy();
});

test("renders run history rows with status labels, notes, and errors", () => {
  const history: RunInfo[] = [
    {
      id: "run-1",
      status: "success",
      startedAtMs: Date.now() - 3_600_000,
      durationMs: 90_000,
      addLines: 12,
      delLines: 3,
      note: "Opened PR #42",
      error: null,
      sessionPk: "sess-1",
    },
    {
      id: "run-2",
      status: "failed",
      startedAtMs: Date.now() - 7_200_000,
      durationMs: 5_000,
      addLines: null,
      delLines: null,
      note: null,
      error: "agent exited with code 1",
      sessionPk: null,
    },
  ];
  seed([makeJob({ history })]);
  render(<JobDetailView id="job-1" />);

  expect(screen.getByText("2 runs · 1 failed")).toBeTruthy();
  expect(screen.getByText("Success")).toBeTruthy();
  expect(screen.getByText("Failed")).toBeTruthy();
  expect(screen.getByText(/Opened PR #42/)).toBeTruthy();
  expect(screen.getByText("agent exited with code 1")).toBeTruthy();
  expect(screen.getByText("1m 30s")).toBeTruthy();
  // Only the run with a session gets the jump-to-session action.
  expect(screen.getAllByRole("button", { name: "Open session" })).toHaveLength(1);
});

test("clicking Run now invokes the runJobNow command with the job id", async () => {
  seed([makeJob()]);
  render(<JobDetailView id="job-1" />);

  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Run now" }));
  });

  expect(runJobNow).toHaveBeenCalledTimes(1);
  expect(runJobNow).toHaveBeenCalledWith("job-1");
});

test("shows a not-found placeholder for an unknown job id", () => {
  seed([makeJob()]);
  render(<JobDetailView id="missing" />);

  expect(screen.getByText("Job not found.")).toBeTruthy();
});

test("a branchless (non-git) job hides the branch chip", () => {
  seed([makeJob({ branch: "" })]);
  render(<JobDetailView id="job-1" />);
  expect(screen.getByText("ryuzi")).toBeTruthy();
  expect(screen.queryByText("main")).toBeNull();
});
