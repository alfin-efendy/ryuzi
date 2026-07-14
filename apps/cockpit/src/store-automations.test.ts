import { afterEach, expect, mock, test } from "bun:test";
import type { AutomationHookDetail, AutomationHookInfo, AutomationHookInput } from "@/bindings";

const hook: AutomationHookInfo = {
  id: "hook-1",
  name: "Notify review",
  triggerKind: "session.end",
  actionKind: "agent.run",
  enabled: true,
  inboundPath: null,
  createdAt: 1,
  updatedAt: 1,
};

const detail: AutomationHookDetail = {
  hook,
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

const listAutomationHooks = mock(async () => ({ status: "ok" as const, data: [hook] }));
const automationHookDetail = mock(async () => ({ status: "ok" as const, data: detail }));
const createAutomationHook = mock(async () => ({ status: "ok" as const, data: hook }));
const updateAutomationHook = mock(async () => ({ status: "ok" as const, data: hook }));
const toggleAutomationHook = mock(async () => ({ status: "ok" as const, data: { ...hook, enabled: false } }));
const deleteAutomationHook = mock(async () => ({ status: "ok" as const, data: [] }));
const testAutomationHook = mock(async () => ({ status: "ok" as const, data: detail }));
let eventListener: ((event: { payload: { event: { kind: string } } }) => void) | null = null;
const unlisten = mock(() => {});
const listen = mock(async (listener: typeof eventListener) => {
  eventListener = listener;
  return unlisten;
});

mock.module("./bindings", () => ({
  commands: {
    listAutomationHooks,
    automationHookDetail,
    createAutomationHook,
    updateAutomationHook,
    toggleAutomationHook,
    deleteAutomationHook,
    testAutomationHook,
  },
  events: { coreEventMsg: { listen } },
}));

const { resetAutomationListenerForTest, useAutomations } = await import("./store-automations");

function reset() {
  resetAutomationListenerForTest();
  eventListener = null;
  useAutomations.setState({ hooks: [], detailsById: {}, loaded: false });
  listAutomationHooks.mockClear();
  automationHookDetail.mockClear();
  createAutomationHook.mockClear();
  updateAutomationHook.mockClear();
  toggleAutomationHook.mockClear();
  deleteAutomationHook.mockClear();
  testAutomationHook.mockClear();
  listen.mockClear();
  unlisten.mockClear();
}

afterEach(reset);

test("loads hooks and toggles a local hook", async () => {
  await useAutomations.getState().load();
  expect(listAutomationHooks).toHaveBeenCalledWith("local");
  expect(useAutomations.getState().hooks).toEqual([hook]);

  await useAutomations.getState().toggle("hook-1", false);
  expect(toggleAutomationHook).toHaveBeenCalledWith("local", "hook-1", false);
  expect(useAutomations.getState().hooks[0]?.enabled).toBe(false);
});

test("retries listener setup after a rejected subscription and refreshes from its event", async () => {
  listen.mockRejectedValueOnce(new Error("listener unavailable"));

  await useAutomations.getState().load();
  expect(listen).toHaveBeenCalledTimes(1);

  await useAutomations.getState().load();
  expect(listen).toHaveBeenCalledTimes(2);
  expect(eventListener).not.toBeNull();

  eventListener?.({ payload: { event: { kind: "automationHookRunChanged" } } });
  await Promise.resolve();
  expect(listAutomationHooks).toHaveBeenCalledTimes(3);
});

test("installs one listener that refreshes only for automation hook runs", async () => {
  await useAutomations.getState().load();
  await useAutomations.getState().load();
  expect(listen).toHaveBeenCalledTimes(1);

  eventListener?.({ payload: { event: { kind: "jobRunChanged" } } });
  expect(listAutomationHooks).toHaveBeenCalledTimes(2);

  eventListener?.({ payload: { event: { kind: "automationHookRunChanged" } } });
  await Promise.resolve();
  expect(listAutomationHooks).toHaveBeenCalledTimes(3);
});

test("caches details and persists hook mutations locally", async () => {
  const input: AutomationHookInput = {
    name: hook.name,
    triggerKind: hook.triggerKind,
    enabled: true,
    action: {
      kind: "agent.run",
      config:
        detail.action.kind === "agent.run"
          ? detail.action.config
          : { projectId: "project-1", branch: "", gatewayId: "local", prompt: "", agentId: null, modelOverride: null, subtask: false },
    },
  };
  await useAutomations.getState().loadDetail("hook-1");
  expect(automationHookDetail).toHaveBeenCalledWith("local", "hook-1");
  expect(useAutomations.getState().detailsById["hook-1"]).toEqual(detail);

  await useAutomations.getState().create(input);
  await useAutomations.getState().update("hook-1", input);
  await useAutomations.getState().testOutbound("hook-1");
  await useAutomations.getState().remove("hook-1");
  expect(createAutomationHook).toHaveBeenCalledWith("local", input);
  expect(updateAutomationHook).toHaveBeenCalledWith("local", "hook-1", input);
  expect(testAutomationHook).toHaveBeenCalledWith("local", "hook-1");
  expect(deleteAutomationHook).toHaveBeenCalledWith("local", "hook-1");
});
