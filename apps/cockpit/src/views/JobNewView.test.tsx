import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { BranchList, CmdError, GatewayInfo, JobInfo, Project, Result } from "@/bindings";

const branchListData: BranchList = { branches: ["main", "develop"], current: "main", detached: false };
const listBranches = mock((): Promise<Result<BranchList, CmdError>> => Promise.resolve({ status: "ok", data: branchListData }));
const createJob = mock((): Promise<Result<JobInfo[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));
const parseNaturalSchedule = mock(async () => null);

mock.module("@/bindings", () => ({
  commands: { listBranches, createJob, parseNaturalSchedule },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));

const { JobNewView } = await import("./JobNewView");
const { useStore } = await import("@/store");
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

function project(overrides: Partial<Project> = {}): Project {
  return {
    projectId: "p1",
    name: "demo",
    workdir: "C:\\code\\demo",
    source: null,
    harness: "native",
    model: null,
    effort: null,
    permMode: "default",
    createdAt: 1,
    isGit: true,
    ...overrides,
  };
}

beforeEach(() => {
  // loaded: true keeps the mount effect from re-hydrating gateways over IPC.
  useGateways.setState({ gateways: [gateway], loaded: true });
  listBranches.mockClear();
  createJob.mockClear();
});

afterEach(cleanup);

test("git project: branches are fetched for the branch picker", async () => {
  useStore.setState({ projects: [project()] });
  render(<JobNewView />);
  await waitFor(() => expect(listBranches).toHaveBeenCalledWith("p1"));
});

test("non-git project: no branch fetch, no branch pill, job creates branchless", async () => {
  useStore.setState({ projects: [project({ isGit: false })] });
  render(<JobNewView />);

  // The Branch combobox trigger (fallback text "main") must be gone. Base UI
  // marks the combobox trigger role="combobox" with aria-label as its
  // accessible name, so the presence check targets that, not role="button".
  expect(screen.queryByRole("combobox", { name: "Branch" })).toBeNull();

  fireEvent.change(screen.getByPlaceholderText("What should the agent do on every run?"), {
    target: { value: "nightly triage" },
  });
  const create = screen.getByRole("button", { name: "Create job" }) as HTMLButtonElement;
  expect(create.disabled).toBe(false);

  fireEvent.click(create);
  await waitFor(() => expect(createJob).toHaveBeenCalledTimes(1));
  expect(createJob).toHaveBeenCalledWith(expect.objectContaining({ projectId: "p1", branch: "" }));
  expect(listBranches).not.toHaveBeenCalled();
});
