import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { CmdError, GatewayInfo, JobInfo, Result } from "@/bindings";

const jobs: JobInfo[] = [
  {
    id: "job-1",
    name: "Nightly triage",
    cron: "0 2 * * *",
    mode: "cron",
    natural: "",
    projectId: "proj-1",
    projectName: "ryuzi",
    branch: "main",
    gateway: "local",
    enabled: true,
    prompt: "Triage issues",
    notifySuccess: true,
    notifyFail: false,
    nextRunMs: null,
    history: [],
  },
];

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

const ok = (data: JobInfo[]): Result<JobInfo[], CmdError> => ({ status: "ok", data });
const listJobs = mock(async () => ok(jobs));
const toggleJob = mock(async () => ok(jobs));

mock.module("@/bindings", () => ({
  commands: { listJobs, toggleJob },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));

const { AutomationsView } = await import("./AutomationsView");
const { useScheduler } = await import("@/store-scheduler");
const { useGateways } = await import("@/store-gateways");

beforeEach(() => {
  useScheduler.setState({ jobs, loaded: true });
  useGateways.setState({ gateways: [gateway], loaded: true });
});

afterEach(cleanup);

test("renders all automation tabs and preserves the Scheduler job list by default", async () => {
  await act(async () => {
    render(<AutomationsView />);
  });
  await waitFor(() => expect(screen.getByText("Nightly triage")).toBeTruthy());

  expect(screen.getByRole("button", { name: "Scheduler" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Hooks" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Commands" })).toBeTruthy();
  expect(screen.getByRole("heading", { name: "Scheduler" })).toBeTruthy();
});

test("initializes the requested Hooks tab and changes tabs locally", async () => {
  render(<AutomationsView initialTab="hooks" />);

  expect(await screen.findByRole("heading", { name: "Hooks" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Commands" }));
  expect(screen.getByText("Select a project to manage project commands")).toBeTruthy();
});

test("uses the requested initial tab after a keyed route change", async () => {
  const renderForTab = (initialTab: "scheduler" | "hooks") => <AutomationsView key={initialTab} initialTab={initialTab} />;
  const { rerender } = render(renderForTab("scheduler"));
  await waitFor(() => expect(screen.getByRole("heading", { name: "Scheduler" })).toBeTruthy());

  rerender(renderForTab("hooks"));

  expect(await screen.findByRole("heading", { name: "Hooks" })).toBeTruthy();
});
