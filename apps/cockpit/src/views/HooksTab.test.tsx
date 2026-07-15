import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { LOCAL_RUNNER } from "@/lib/session-key";
import { useNav } from "@/store-nav";
import { useStore } from "@/store";
import type { AutomationHookDetail, AutomationHookInfo, AutomationHookInput, EndpointStatusInfo, Project } from "@/bindings";

const project: Project = {
  projectId: "project-1",
  name: "Cockpit",
  workdir: "C:/cockpit",
  source: null,
  model: null,
  effort: null,
  permMode: "default",
  createdAt: null,
  isGit: true,
};

const endpoint: EndpointStatusInfo = {
  running: false,
  port: 4210,
  baseUrl: "http://127.0.0.1:4210/v1",
  autostart: false,
  keychainStatus: "ok",
};
const inbound: AutomationHookInfo = {
  id: "inbound-1",
  name: "Inbound review",
  triggerKind: "webhook.inbound",
  actionKind: "agent.run",
  enabled: true,
  inboundPath: "wh_x",
  createdAt: 1,
  updatedAt: 1,
};
const outbound: AutomationHookInfo = {
  id: "outbound-1",
  name: "Post release",
  triggerKind: "session.end",
  actionKind: "webhook.outbound",
  enabled: true,
  inboundPath: null,
  createdAt: 1,
  updatedAt: 1,
};

const inboundDetail: AutomationHookDetail = {
  hook: inbound,
  action: {
    kind: "agent.run",
    config: {
      projectId: "project-1",
      branch: "main",
      gatewayId: "local",
      prompt: "Review $EVENT",
      agentId: null,
      modelOverride: null,
      subtask: false,
    },
  },
  runs: [],
};
const disabled = { ...inbound, id: "disabled-1", name: "Disabled review", enabled: false };
const disabledDetail: AutomationHookDetail = {
  ...inboundDetail,
  hook: disabled,
};
const outboundDetail: AutomationHookDetail = {
  hook: outbound,
  action: {
    kind: "webhook.outbound",
    config: {
      url: "https://example.com/release",
      method: "POST",
      headers: [{ name: "Authorization", configured: true }],
      // biome-ignore lint/suspicious/noTemplateCurlyInString: fixture exercises the literal backend placeholder syntax.
      payloadTemplate: '{"event":"${event}"}',
    },
  },
  runs: [
    {
      id: "run-1",
      hookId: "outbound-1",
      status: "failed",
      sessionPk: "session-1",
      error: "delivery failed",
      attemptCount: 3,
      lastHttpStatus: 502,
      queuedAt: 1,
      startedAt: 2,
      finishedAt: 3,
      attempts: [
        { runId: "run-1", ordinal: 1, startedAt: 1, finishedAt: 2, httpStatus: 502, error: "bad gateway" },
        { runId: "run-1", ordinal: 2, startedAt: 2, finishedAt: 3, httpStatus: 502, error: "bad gateway" },
        { runId: "run-1", ordinal: 3, startedAt: 3, finishedAt: 4, httpStatus: 502, error: "bad gateway" },
      ],
    },
  ],
};

const endpointStatus = mock(async () => ({ status: "ok" as const, data: endpoint }));
const listEndpointKeys = mock(async () => ({ status: "ok" as const, data: [] }));
const listAutomationHooks = mock(async () => ({ status: "ok" as const, data: [inbound, outbound] }));
const automationHookDetail = mock(async (_runner: string, id: string) => ({
  status: "ok" as const,
  data: id === inbound.id ? inboundDetail : outboundDetail,
}));
const testAutomationHook = mock(async () => ({ status: "ok" as const, data: outboundDetail }));
const nativeAgents = mock(async () => ({ status: "error" as const, error: { message: "not needed by this test" } }));
const createAutomationHook = mock<(runner: string, input: AutomationHookInput) => Promise<{ status: "ok"; data: AutomationHookInfo }>>(
  async () => ({ status: "ok" as const, data: inbound }),
);
const updateAutomationHook = mock<
  (runner: string, id: string, input: AutomationHookInput) => Promise<{ status: "ok"; data: AutomationHookInfo }>
>(async () => ({ status: "ok" as const, data: outbound }));
const listMessages = mock(async () => ({ status: "ok" as const, data: [] }));

mock.module("@/bindings", () => ({
  commands: {
    endpointStatus,
    listEndpointKeys,
    listAutomationHooks,
    automationHookDetail,
    testAutomationHook,
    nativeAgents,
    createAutomationHook,
    updateAutomationHook,
    listMessages,
  },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));

const { HooksTab } = await import("./HooksTab");
const { useAutomations } = await import("@/store-automations");
const { useEndpoint } = await import("@/store-endpoint");
const { useNative } = await import("@/store-native");

afterEach(() => {
  cleanup();
  useAutomations.setState({ hooks: [], detailsById: {}, loaded: false });
  useEndpoint.setState({ status: null, keys: [], loaded: false });
  useNative.setState({ agentsByProject: {} });
  useStore.setState({ focusedSession: null });
  useNav.setState({ history: { back: [], current: { kind: "automations", tab: "hooks" }, forward: [] } });
  endpointStatus.mockClear();
  listAutomationHooks.mockClear();
  listAutomationHooks.mockImplementation(async () => ({ status: "ok" as const, data: [inbound, outbound] }));
  automationHookDetail.mockImplementation(async (_runner: string, id: string) => ({
    status: "ok" as const,
    data: id === inbound.id ? inboundDetail : outboundDetail,
  }));
  testAutomationHook.mockClear();
  nativeAgents.mockClear();
  createAutomationHook.mockClear();
  updateAutomationHook.mockClear();
});

test("keeps a typed name when edit detail resolves after user input", async () => {
  const resolvers: Array<(result: { status: "ok"; data: AutomationHookDetail }) => void> = [];
  automationHookDetail.mockImplementation(async (_runner: string, id: string) =>
    id === outbound.id ? await new Promise((resolve) => resolvers.push(resolve)) : { status: "ok" as const, data: inboundDetail },
  );

  render(<HooksTab projects={[project]} />);
  await screen.findByText("Post release");
  await waitFor(() => expect(resolvers.length).toBeGreaterThan(0));
  fireEvent.click(screen.getByRole("button", { name: "Edit Post release" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: "Typed name" } });

  for (const resolve of resolvers) resolve({ status: "ok", data: outboundDetail });

  await waitFor(() => expect(useAutomations.getState().detailsById[outbound.id]).toEqual(outboundDetail));
  expect((screen.getByRole("textbox", { name: "Name" }) as HTMLInputElement).value).toBe("Typed name");
});

test("shows a Models callout instead of starting a stopped inbound endpoint", async () => {
  render(<HooksTab projects={[project]} />);
  await screen.findByText("Inbound review");
  fireEvent.click(screen.getByRole("button", { name: "Edit Inbound review" }));
  expect(await screen.findByText("The local endpoint is stopped.")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Open Models" })).toBeTruthy();
  expect(endpointStatus).toHaveBeenCalledWith("local");
});

test("only shows an inbound URL after the saved hook has an endpoint path", async () => {
  useEndpoint.setState({ status: { ...endpoint, running: true }, loaded: true });
  render(<HooksTab projects={[project]} />);
  await screen.findByText("Inbound review");
  fireEvent.click(screen.getByRole("button", { name: "Edit Inbound review" }));
  expect(await screen.findByText("http://127.0.0.1:4210/v1/automations/hooks/wh_x")).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  fireEvent.click(screen.getByRole("button", { name: "New hook" }));
  expect(screen.queryByText("http://127.0.0.1:4210/v1/automations/hooks/wh_x")).toBeNull();
});

test("opens a hook run's local session", async () => {
  render(<HooksTab projects={[project]} />);
  await screen.findByText("Post release");
  fireEvent.click(screen.getByRole("button", { name: "Edit Post release" }));
  fireEvent.click(await screen.findByRole("button", { name: "Open session session-1" }));

  expect(useStore.getState().focusedSession).toEqual({ runnerId: LOCAL_RUNNER, pk: "session-1" });
  expect(useNav.getState().history.current).toEqual({ kind: "session" });
});

test("submits enabled true for a newly created hook", async () => {
  render(<HooksTab projects={[project]} />);
  await screen.findByText("Inbound review");
  fireEvent.click(screen.getByRole("button", { name: "New hook" }));
  fireEvent.change(screen.getByRole("textbox", { name: "Name" }), { target: { value: "New review" } });
  fireEvent.change(screen.getByRole("textbox", { name: "Prompt" }), { target: { value: "Review $EVENT" } });
  fireEvent.click(screen.getByRole("button", { name: "Save hook" }));

  await waitFor(() => expect(createAutomationHook).toHaveBeenCalledTimes(1));
  expect((createAutomationHook.mock.calls[0]?.[1] as AutomationHookInput).enabled).toBe(true);
});

test("submits the selected native agent name for the hook project", async () => {
  useNative.setState({
    agentsByProject: { "project-1": [{ name: "native-reviewer", description: "Reviews changes", mode: "subagent", builtin: true }] },
  });
  render(<HooksTab projects={[project]} />);
  await screen.findByText("Inbound review");
  fireEvent.click(screen.getByRole("button", { name: "Edit Inbound review" }));

  const agent = await screen.findByRole("combobox", { name: "Agent" });
  fireEvent.click(agent);
  fireEvent.click(await screen.findByRole("option", { name: /native-reviewer/ }));
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Agent" }).textContent).toContain("native-reviewer"));
  fireEvent.click(screen.getByRole("button", { name: "Save hook" }));

  await waitFor(() => expect(nativeAgents).toHaveBeenCalledWith("local", "project-1"));
  expect(updateAutomationHook).toHaveBeenCalledWith(
    "local",
    "inbound-1",
    expect.objectContaining({ action: expect.objectContaining({ config: expect.objectContaining({ agentId: "native-reviewer" }) }) }),
  );
});

test("preserves enabled false when editing a disabled hook", async () => {
  listAutomationHooks.mockImplementation(async () => ({ status: "ok" as const, data: [disabled] }));
  automationHookDetail.mockImplementation(async () => ({ status: "ok" as const, data: disabledDetail }));
  render(<HooksTab projects={[project]} />);
  await screen.findByText("Disabled review");
  fireEvent.click(screen.getByRole("button", { name: "Edit Disabled review" }));
  await screen.findByRole("textbox", { name: "Prompt" });
  fireEvent.click(screen.getByRole("button", { name: "Save hook" }));

  await waitFor(() => expect(updateAutomationHook).toHaveBeenCalledTimes(1));
  expect((updateAutomationHook.mock.calls[0]?.[2] as AutomationHookInput).enabled).toBe(false);
});

test("outbound editor exposes only POST and sends POST when updating", async () => {
  render(<HooksTab projects={[project]} />);
  await screen.findByText("Post release");
  fireEvent.click(screen.getByRole("button", { name: "Edit Post release" }));
  await screen.findByText("Authorization configured");

  fireEvent.click(screen.getByRole("combobox", { name: "Method" }));
  expect(screen.getAllByRole("option")).toHaveLength(1);
  expect(screen.getByRole("option", { name: "POST" })).toBeTruthy();
  expect(screen.queryByRole("option", { name: "PUT" })).toBeNull();
  expect(screen.queryByRole("option", { name: "PATCH" })).toBeNull();
  fireEvent.keyDown(screen.getByRole("listbox"), { key: "Escape" });

  fireEvent.click(screen.getByRole("button", { name: "Save hook" }));
  await waitFor(() => expect(updateAutomationHook).toHaveBeenCalledTimes(1));
  const input = updateAutomationHook.mock.calls[0]?.[2] as AutomationHookInput;
  expect(input.action).toEqual({
    kind: "webhook.outbound",
    config: {
      url: "https://example.com/release",
      method: "POST",
      headers: [{ name: "Authorization", value: "" }],
      // biome-ignore lint/suspicious/noTemplateCurlyInString: fixture exercises the literal backend placeholder syntax.
      payloadTemplate: '{"event":"${event}"}',
    },
  });
});

test("redacts outbound header values, tests delivery, and shows three attempts in history", async () => {
  render(<HooksTab projects={[project]} />);
  await screen.findByText("Post release");
  fireEvent.click(screen.getByRole("button", { name: "Edit Post release" }));
  await screen.findByText("Authorization configured");
  expect(screen.queryByText("secret-token")).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "Test delivery" }));
  await waitFor(() => expect(testAutomationHook).toHaveBeenCalledWith("local", "outbound-1"));
  expect(await screen.findByText("HTTP 502")).toBeTruthy();
  expect(screen.getByText("Attempt 3 · HTTP 502")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Open session session-1" })).toBeTruthy();
});
