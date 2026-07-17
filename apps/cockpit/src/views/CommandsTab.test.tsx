import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { CommandInfo, Project, ProjectCommandInfo, SelectableModelInfo } from "@/bindings";

const command: ProjectCommandInfo = {
  name: "audit",
  description: "Review the current change",
  template: "Review $ARGUMENTS and $1",
  agent: "reviewer",
  model: "free",
  subtask: true,
  revision: "rev-1",
};

const project: Project = {
  projectId: "p1",
  name: "Cockpit",
  workdir: "C:/cockpit",
  source: null,
  model: null,
  effort: null,
  permMode: "default",
  createdAt: null,
  isGit: true,
};

const modelOption: SelectableModelInfo = {
  kind: "namedRoute",
  requestValue: "free",
  displayName: "Smart",
  preferenceKey: null,
  supported: [],
  configuredDefault: null,
  resolvedDefault: null,
  defaultSource: "none",
};

const createdCommand = { ...command, name: "ship", template: "Ship $ARGUMENTS" };
const effectiveCommands: CommandInfo[] = [
  {
    name: "audit",
    description: "Project audit",
    agent: null,
    model: null,
    subtask: false,
    origin: "project",
    effective: true,
    shadowsGlobal: true,
  },
  {
    name: "deploy",
    description: "Deploy everywhere",
    agent: null,
    model: null,
    subtask: false,
    origin: "global",
    effective: true,
    shadowsGlobal: false,
  },
  {
    name: "init",
    description: "Initialize the project",
    agent: null,
    model: null,
    subtask: false,
    origin: "builtin",
    effective: true,
    shadowsGlobal: false,
  },
];
const listProjectCommands = mock(async () => ({ status: "ok" as const, data: [command] }));
const nativeCommands = mock(async () => ({ status: "ok" as const, data: effectiveCommands }));
const createProjectCommand = mock(async () => ({ status: "ok" as const, data: createdCommand }));
const updateProjectCommand = mock(async () => ({ status: "ok" as const, data: command }));
const deleteProjectCommand = mock(async () => ({ status: "ok" as const, data: null }));
const nativeAgents = mock(async () => ({
  status: "ok" as const,
  data: [{ name: "reviewer", description: "Reviews changes", mode: "subagent", builtin: true }],
}));

mock.module("@/bindings", () => ({
  commands: { listProjectCommands, createProjectCommand, updateProjectCommand, deleteProjectCommand, nativeAgents, nativeCommands },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));

const { CommandsTab, projectCommandNameError, projectCommandPreview } = await import("./CommandsTab");
const { useNative } = await import("@/store-native");
const { useAgents } = await import("@/store-agents");
const nativeDeleteProjectCommand = useNative.getState().deleteProjectCommand;

afterEach(() => {
  cleanup();
  useNative.setState({
    projectCommandsByProject: {},
    commandsByProject: {},
    agentsByProject: {},
    deleteProjectCommand: nativeDeleteProjectCommand,
  });
  useAgents.setState({ models: [] });
  listProjectCommands.mockClear();
  createProjectCommand.mockClear();
  updateProjectCommand.mockClear();
  deleteProjectCommand.mockClear();
  nativeAgents.mockClear();
  nativeCommands.mockClear();
});

test("disables command creation until a project is selected", () => {
  render(<CommandsTab projects={[]} defaultProjectId={null} />);

  expect(screen.getByText("Select a project to manage project commands")).toBeTruthy();
  expect(screen.getByRole("button", { name: "New command" }).hasAttribute("disabled")).toBe(true);
});

test("validates backend command names and previews positional placeholders", () => {
  expect(projectCommandNameError("Review")).toContain("lowercase");
  expect(projectCommandNameError("init")).toContain("Built-in");
  expect(projectCommandNameError("init", true)).toBeNull();
  expect(projectCommandNameError("team/review")).toBeNull();
  expect(projectCommandPreview("review", "Review $ARGUMENTS; compare $1 with $2")).toBe(
    "/review <arguments>\nReview <arguments>; compare <argument 1> with <argument 2>",
  );
});

test("saves an existing reserved command after editing its template", async () => {
  const initCommand: ProjectCommandInfo = {
    ...command,
    name: "init",
    description: "Initialize this project",
    template: "Initial template",
  };
  useNative.setState({ projectCommandsByProject: { p1: [initCommand] } });
  listProjectCommands.mockResolvedValueOnce({ status: "ok", data: [initCommand] });
  render(<CommandsTab projects={[project]} defaultProjectId="p1" />);

  await screen.findByText("Initialize this project");
  fireEvent.click(screen.getByRole("button", { name: "Edit /init" }));
  expect((screen.getByLabelText("Name") as HTMLInputElement).disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Template"), { target: { value: "Updated template" } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() =>
    expect(updateProjectCommand).toHaveBeenCalledWith(
      "local",
      "p1",
      "init",
      initCommand.revision,
      expect.objectContaining({ template: "Updated template" }),
    ),
  );
});

test("renders global and built-in commands as read-only with visible origins", async () => {
  render(<CommandsTab projects={[project]} defaultProjectId="p1" />);

  expect(await screen.findByText("/deploy")).toBeTruthy();
  expect(screen.getByText("Global")).toBeTruthy();
  expect(screen.getByText("/init")).toBeTruthy();
  expect(screen.getByText("Built-in")).toBeTruthy();
  const projectRow = screen.getByText("/audit").closest(".min-h-\\[88px\\]");
  expect(projectRow?.textContent).toContain("Project");
  expect(screen.queryByRole("button", { name: "Edit /deploy" })).toBeNull();
  expect(screen.queryByRole("button", { name: "Delete /deploy" })).toBeNull();
  expect(screen.getByText("Overrides global")).toBeTruthy();
});

test("shows every colliding source with its effective status", async () => {
  const catalog: CommandInfo[] = [
    {
      name: "ship",
      description: "Project ship",
      agent: null,
      model: null,
      subtask: false,
      origin: "project",
      effective: true,
      shadowsGlobal: true,
    },
    {
      name: "ship",
      description: "Global ship",
      agent: null,
      model: null,
      subtask: false,
      origin: "global",
      effective: false,
      shadowsGlobal: false,
    },
    {
      name: "init",
      description: "Project init",
      agent: null,
      model: null,
      subtask: false,
      origin: "project",
      effective: false,
      shadowsGlobal: true,
    },
    {
      name: "init",
      description: "Global init",
      agent: null,
      model: null,
      subtask: false,
      origin: "global",
      effective: false,
      shadowsGlobal: false,
    },
    {
      name: "init",
      description: "Built-in init",
      agent: null,
      model: null,
      subtask: false,
      origin: "builtin",
      effective: true,
      shadowsGlobal: false,
    },
  ];
  const projectCommands = [
    { ...command, name: "ship", description: "Project ship" },
    { ...command, name: "init", description: "Project init" },
  ];
  listProjectCommands.mockResolvedValueOnce({ status: "ok", data: projectCommands });
  nativeCommands.mockResolvedValueOnce({ status: "ok", data: catalog });

  render(<CommandsTab projects={[project]} defaultProjectId="p1" />);

  expect(await screen.findByText("Global ship")).toBeTruthy();
  expect(screen.getByText("Global init")).toBeTruthy();
  expect(screen.getByText("Built-in init")).toBeTruthy();
  expect(screen.getByText("Overrides global")).toBeTruthy();
  expect(screen.getAllByText("Shadowed by built-in")).toHaveLength(2);
  expect(screen.getAllByText("Shadowed by project")).toHaveLength(1);
  expect(screen.getAllByText("Effective")).toHaveLength(1);
  expect(screen.queryByRole("button", { name: "Edit /ship" })).toBeTruthy();
  expect(screen.getAllByRole("button", { name: "Edit /init" })).toHaveLength(1);
});

test("opens deletion confirmation from a trigger and confirms or cancels the requested command", async () => {
  render(<CommandsTab projects={[project]} defaultProjectId="p1" />);
  await screen.findByText("Review the current change");

  const trigger = screen.getByRole("button", { name: "Delete /audit" });
  fireEvent.click(trigger);
  expect(await screen.findByRole("dialog", { name: "Delete /audit?" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(document.activeElement).toBe(trigger));
  expect(deleteProjectCommand).not.toHaveBeenCalled();

  fireEvent.click(trigger);
  fireEvent.click(screen.getByRole("button", { name: "Delete command" }));
  await waitFor(() => expect(deleteProjectCommand).toHaveBeenCalledWith("local", "p1", "audit", "rev-1"));
});

test("closes deletion confirmation after a conflict reloads the latest command", async () => {
  const deleteConflict = mock(async () => ({ status: "conflict" as const, message: "Command changed externally." }));
  useNative.setState({ deleteProjectCommand: deleteConflict });
  render(<CommandsTab projects={[project]} defaultProjectId="p1" />);
  await screen.findByText("Review the current change");

  fireEvent.click(screen.getByRole("button", { name: "Delete /audit" }));
  expect(await screen.findByRole("dialog", { name: "Delete /audit?" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Delete command" }));

  await waitFor(() => expect(deleteConflict).toHaveBeenCalledWith("local", "p1", command));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Delete /audit?" })).toBeNull());
});

test("loads agents and model registry for the selected local project and submits selected overrides", async () => {
  useAgents.setState({ models: [modelOption] });
  useNative.setState({ agentsByProject: { p1: [{ name: "reviewer", description: "Reviews changes", mode: "subagent", builtin: true }] } });
  render(<CommandsTab projects={[project]} defaultProjectId="p1" />);
  await waitFor(() => expect(nativeAgents).toHaveBeenCalledWith("local", "p1"));
  await waitFor(() => expect(useNative.getState().agentsByProject.p1).toHaveLength(1));
  fireEvent.click(screen.getByRole("button", { name: "New command" }));
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "ship" } });
  fireEvent.change(screen.getByLabelText("Template"), { target: { value: "Ship $ARGUMENTS" } });

  const agent = await screen.findByRole("combobox", { name: "Agent" });
  fireEvent.click(agent);
  fireEvent.click(await screen.findByRole("option", { name: /reviewer/ }));
  const model = screen.getByRole("combobox", { name: "Model" });
  fireEvent.click(model);
  fireEvent.click(await screen.findByRole("option", { name: "Smart" }));
  fireEvent.click(screen.getByRole("button", { name: "Create" }));

  await waitFor(() => expect(nativeAgents).toHaveBeenCalledWith("local", "p1"));
  expect(createProjectCommand).toHaveBeenCalledWith(
    "local",
    "p1",
    expect.objectContaining({ name: "ship", agent: "reviewer", model: "free" }),
  );
});

test("keeps an explicit null default project unselected and focuses enabled edit description", async () => {
  render(<CommandsTab projects={[project]} defaultProjectId={null} />);
  expect(screen.getAllByText("Select a project to manage project commands")[0]).toBeTruthy();
  cleanup();

  render(<CommandsTab projects={[project]} defaultProjectId="p1" />);
  await screen.findByText("Review the current change");
  fireEvent.click(screen.getByRole("button", { name: "Edit /audit" }));
  await waitFor(() => expect(document.activeElement).toBe(screen.getByLabelText("Description")));
});
test("loads project rows and creates a command through the generated API", async () => {
  render(<CommandsTab projects={[project]} defaultProjectId="p1" />);

  await waitFor(() => expect(screen.getByText("Review the current change")).toBeTruthy());
  expect(listProjectCommands).toHaveBeenCalledWith("local", "p1");

  fireEvent.click(screen.getByRole("button", { name: "New command" }));
  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "ship" } });
  fireEvent.change(screen.getByLabelText("Template"), { target: { value: "Ship $ARGUMENTS" } });
  fireEvent.click(screen.getByRole("button", { name: "Create" }));

  await waitFor(() => expect(createProjectCommand).toHaveBeenCalledWith("local", "p1", expect.objectContaining({ name: "ship" })));
});
