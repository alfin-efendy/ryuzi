import { afterEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { CmdError, Project, Result, ToolPolicyRow } from "@/bindings";

// Mock the Tauri boundary before the component (and the store it pulls in) load.
let seededRules: ToolPolicyRow[] = [];
const ok = <T,>(data: T): Result<T, CmdError> => ({ status: "ok", data });

const listToolPolicies = mock(async () => ok(seededRules));
const deleteToolPolicy = mock(async () => ok(null));

mock.module("@/bindings", () => ({
  commands: { listToolPolicies, deleteToolPolicy },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));

const { PermissionsCard } = await import("./PermissionsCard");
const { useStore } = await import("@/store");

function makeProject(overrides: Partial<Project> = {}): Project {
  return {
    projectId: "p1",
    name: "ryuzi",
    workdir: "/tmp/ryuzi",
    source: null,
    harness: "claude",
    model: null,
    effort: null,
    permMode: "default",
    createdAt: null,
    ...overrides,
  } as Project;
}

function seed(rules: ToolPolicyRow[], projects: Project[] = []) {
  seededRules = rules;
  useStore.setState({ projects });
}

afterEach(() => {
  cleanup();
  listToolPolicies.mockClear();
  deleteToolPolicy.mockClear();
});

test("shows the empty state when there are no saved rules", async () => {
  seed([]);
  await act(async () => {
    render(<PermissionsCard />);
  });

  expect(screen.getByText("No saved rules.")).toBeTruthy();
});

test("lists persisted rules with the project name and decision badge", async () => {
  seed(
    [
      { projectId: "p1", tool: "Bash", decision: "allowAlways" },
      { projectId: "p2", tool: "Edit", decision: "rejectAlways" },
    ],
    [makeProject({ projectId: "p1", name: "ryuzi" })],
  );

  await act(async () => {
    render(<PermissionsCard />);
  });

  expect(screen.getByText("Bash")).toBeTruthy();
  expect(screen.getByText("ryuzi")).toBeTruthy();
  expect(screen.getByText("Always allow")).toBeTruthy();

  expect(screen.getByText("Edit")).toBeTruthy();
  // Falls back to the raw project id when the project is unknown.
  expect(screen.getByText("p2")).toBeTruthy();
  expect(screen.getByText("Always deny")).toBeTruthy();
});

test("clicking the delete button revokes the rule and refetches", async () => {
  seed([{ projectId: "p1", tool: "Bash", decision: "allowAlways" }], [makeProject()]);

  await act(async () => {
    render(<PermissionsCard />);
  });

  seededRules = []; // what the refetch after delete should observe

  await act(async () => {
    fireEvent.click(screen.getByRole("button", { name: "Remove rule for Bash" }));
  });

  expect(deleteToolPolicy).toHaveBeenCalledWith("p1", "Bash");
  expect(listToolPolicies).toHaveBeenCalledTimes(2);
  expect(screen.getByText("No saved rules.")).toBeTruthy();
});
