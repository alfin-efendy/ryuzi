import type { CmdError, Project, Result } from "@/bindings";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { LOCAL_RUNNER } from "@/lib/session-key";

const clonedProject: Project = {
  projectId: "p9",
  name: "repo",
  workdir: "C:\\proj\\repo",
  source: "https://github.com/user/repo.git",
  model: null,
  effort: null,
  permMode: "default",
  createdAt: 1,
  isGit: true,
};

const pickDirectory = mock((): Promise<string | null> => Promise.resolve("C:\\code\\demo"));
const connectProject = mock((): Promise<Result<Project, CmdError>> => Promise.resolve({ status: "ok", data: clonedProject }));
const cloneProject = mock((): Promise<Result<Project, CmdError>> => Promise.resolve({ status: "ok", data: clonedProject }));
const getSetting = mock((): Promise<Result<string | null, CmdError>> => Promise.resolve({ status: "ok", data: "C:\\proj" }));
const listProjects = mock((): Promise<Result<Project[], CmdError>> => Promise.resolve({ status: "ok", data: [] }));
const listSessions = mock(() => Promise.resolve({ status: "ok", data: [] }));
// refresh() (awaited directly by addProject()/cloneProject() on success) always
// fans out to listGateways too — unmocked it rejects and refresh() never resolves.
const listGateways = mock(() => Promise.resolve({ status: "ok", data: [] }));

mock.module("@/bindings", () => ({
  commands: { pickDirectory, connectProject, cloneProject, getSetting, listProjects, listSessions, listGateways },
  events: { coreEventMsg: { listen: mock(() => Promise.resolve(() => {})) } },
}));

const { AddProjectModal } = await import("./AddProjectModal");

beforeEach(() => {
  pickDirectory.mockClear();
  connectProject.mockClear();
  cloneProject.mockClear();
  getSetting.mockClear();
  listProjects.mockClear();
  listSessions.mockClear();
  listGateways.mockClear();
});

afterEach(cleanup);

test("renders nothing while closed", () => {
  render(<AddProjectModal open={false} onClose={() => {}} />);
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("open-folder mode connects the picked directory under its basename", async () => {
  const onClose = mock(() => {});
  render(<AddProjectModal open onClose={onClose} />);

  fireEvent.click(screen.getByRole("button", { name: "Choose folder" }));

  await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  expect(pickDirectory).toHaveBeenCalledTimes(1);
  expect(connectProject).toHaveBeenCalledWith(LOCAL_RUNNER, "C:\\code\\demo", "demo");
});

test("clone mode defaults the destination from the projects_root setting and submits", async () => {
  const onClose = mock(() => {});
  render(<AddProjectModal open onClose={onClose} />);

  fireEvent.click(screen.getByRole("radio", { name: "Clone from URL" }));
  await waitFor(() => expect((screen.getByPlaceholderText("Projects folder") as HTMLInputElement).value).toBe("C:\\proj"));

  const clone = screen.getByRole("button", { name: "Clone" }) as HTMLButtonElement;
  expect(clone.disabled).toBe(true); // no URL yet

  fireEvent.change(screen.getByLabelText("Repository URL"), {
    target: { value: "https://github.com/user/repo.git" },
  });
  expect(clone.disabled).toBe(false);

  fireEvent.click(clone);
  await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
  expect(cloneProject).toHaveBeenCalledWith(LOCAL_RUNNER, "https://github.com/user/repo.git", "C:\\proj");
});

test("Browse overrides the clone destination for this clone only", async () => {
  render(<AddProjectModal open onClose={() => {}} />);

  fireEvent.click(screen.getByRole("radio", { name: "Clone from URL" }));
  await waitFor(() => expect((screen.getByPlaceholderText("Projects folder") as HTMLInputElement).value).toBe("C:\\proj"));

  pickDirectory.mockResolvedValueOnce("D:\\elsewhere");
  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await waitFor(() => expect((screen.getByPlaceholderText("Projects folder") as HTMLInputElement).value).toBe("D:\\elsewhere"));
});
