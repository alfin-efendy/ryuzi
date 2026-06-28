// apps/ide/test/screens.test.tsx
import { test, expect, mock, beforeAll, afterAll, beforeEach, afterEach } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
import type { StartSessionRequest, ContinueSessionRequest } from "@harness/protocol";
import type { HarnessBridge } from "../src/shared/ipc-contract";

beforeAll(() => {
  if (!globalThis.document) GlobalRegistrator.register();
  // eslint-disable-next-line @typescript-eslint/no-explicit-any
  (globalThis as Record<string, unknown>).IS_REACT_ACT_ENVIRONMENT = true;
});
afterAll(() => {
  GlobalRegistrator.unregister();
});

import type React from "react";
import { act } from "react";
import { createRoot } from "react-dom/client";
import type { Root } from "react-dom/client";
import { useStore } from "../src/renderer/store";
import { ProjectsSessionsTree } from "../src/renderer/screens/ProjectsSessionsTree";
import { SessionTranscript } from "../src/renderer/screens/SessionTranscript";

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/** Access the React synthetic-event props object attached to a DOM element. */
function getReactProps(el: Element): Record<string, unknown> | null {
  const key = Object.keys(el).find((k) => k.startsWith("__reactProps"));
  return key ? ((el as unknown as Record<string, unknown>)[key] as Record<string, unknown>) : null;
}

function makeHarness(): HarnessBridge {
  return {
    listProjects: mock(async () => []),
    getProject: mock(async (_id: string) => undefined),
    listSessions: mock(async (_projectId?: string) => []),
    startSession: mock(async (_req: StartSessionRequest) => ({
      sessionPk: "",
      projectId: "",
      title: "",
      status: "running" as const,
    })),
    continueSession: mock(async (_req: ContinueSessionRequest) => undefined),
    stopSession: mock(async (_pk: string) => undefined),
    endSession: mock(async (_pk: string) => undefined),
    getConnId: mock(async () => null as string | null),
    onEvent: mock((_cb: Parameters<HarnessBridge["onEvent"]>[0]) => () => {}),
    onConnectionChange: mock((_cb: Parameters<HarnessBridge["onConnectionChange"]>[0]) => () => {}),
  };
}

function resetStore() {
  const s = useStore.getState();
  s.setProjects([]);
  s.setSessions([]);
  s.setConnId(null);
  s.setActive(null);
}

// Track active roots so we can unmount between tests
let activeRoots: Root[] = [];
let activeContainers: HTMLElement[] = [];

async function renderInto(element: React.ReactElement): Promise<HTMLElement> {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  activeRoots.push(root);
  activeContainers.push(el);
  await act(async () => {
    root.render(element);
  });
  return el;
}

/**
 * Simulate a controlled input: set the DOM value, fire React onChange to update
 * component state, then fire onKeyDown Enter in a separate act() so React state
 * has flushed before the send() handler reads it.
 */
async function typeAndSubmit(input: HTMLInputElement, text: string) {
  await act(async () => {
    (input as unknown as { value: string }).value = text;
    const props = getReactProps(input);
    if (props?.onChange) {
      (props.onChange as (e: { target: HTMLInputElement; currentTarget: HTMLInputElement }) => void)({
        target: input,
        currentTarget: input,
      });
    }
  });
  await act(async () => {
    const props = getReactProps(input);
    if (props?.onKeyDown) {
      (props.onKeyDown as (e: { key: string }) => void)({ key: "Enter" });
    }
  });
}

beforeEach(() => {
  window.harness = makeHarness();
  resetStore();
});

afterEach(async () => {
  // Unmount all roots to prevent stale subscriptions causing act() warnings in the next test
  for (const root of activeRoots) {
    await act(async () => {
      root.unmount();
    });
  }
  for (const container of activeContainers) {
    container.remove();
  }
  activeRoots = [];
  activeContainers = [];
});

// ---------------------------------------------------------------------------
// Existing tree test
// ---------------------------------------------------------------------------

test("tree renders projects and their sessions with status", async () => {
  useStore.getState().setProjects([{ projectId: "p1", name: "demo", workdir: "/w", harness: "fake", permMode: "default" }]);
  useStore.getState().setSessions([{ sessionPk: "s1", projectId: "p1", title: "fix bug", status: "running" }]);
  const el = await renderInto(<ProjectsSessionsTree />);
  expect(el.textContent).toContain("demo");
  expect(el.textContent).toContain("fix bug");
});

// ---------------------------------------------------------------------------
// SessionTranscript – start path + carry-forward
// ---------------------------------------------------------------------------

test("start path: startSession called with right args and carry-forward refreshes store", async () => {
  // Seed store: one project, connId, no active session
  useStore.getState().setProjects([{ projectId: "p1", name: "demo", workdir: "/w", harness: "fake", permMode: "default" }]);
  useStore.getState().setConnId("c1");
  useStore.getState().setActive(null);

  const startMock = mock(async (_req: StartSessionRequest) => ({
    sessionPk: "s9",
    projectId: "p1",
    title: "do x",
    status: "running" as const,
  }));
  const listMock = mock(async (_projectId?: string) => [{ sessionPk: "s9", projectId: "p1", title: "do x", status: "running" as const }]);
  window.harness.startSession = startMock;
  window.harness.listSessions = listMock;

  const el = await renderInto(<SessionTranscript />);

  const input = el.querySelector("input") as HTMLInputElement;
  expect(input).toBeTruthy();

  await typeAndSubmit(input, "hello world");

  // Flush remaining async work (startSession + listSessions promises)
  await act(async () => {});

  // startSession called once with the correct payload
  expect(startMock.mock.calls.length).toBe(1);
  const startCall = startMock.mock.calls[0];
  expect(startCall).toBeDefined();
  expect(startCall![0]).toEqual({
    projectId: "p1",
    prompt: "hello world",
    surface: { gateway: "ide", conversationId: "c1" },
  });

  // carry-forward: listSessions refreshed, store updated
  expect(listMock.mock.calls.length).toBeGreaterThan(0);
  expect(useStore.getState().sessions.some((s) => s.sessionPk === "s9")).toBe(true);
  expect(useStore.getState().activeSessionPk).toBe("s9");
});

// ---------------------------------------------------------------------------
// SessionTranscript – continue path
// ---------------------------------------------------------------------------

test("continue path: continueSession called with active session pk and prompt", async () => {
  useStore.getState().setProjects([{ projectId: "p1", name: "demo", workdir: "/w", harness: "fake", permMode: "default" }]);
  useStore.getState().setSessions([{ sessionPk: "s1", projectId: "p1", title: "fix bug", status: "running" }]);
  useStore.getState().setActive("s1");
  useStore.getState().setConnId("c1");

  const continueMock = mock(async (_req: ContinueSessionRequest) => undefined);
  window.harness.continueSession = continueMock;

  const el = await renderInto(<SessionTranscript />);

  const input = el.querySelector("input") as HTMLInputElement;
  expect(input).toBeTruthy();

  await typeAndSubmit(input, "keep going");

  await act(async () => {});

  expect(continueMock.mock.calls.length).toBe(1);
  const continueCall = continueMock.mock.calls[0];
  expect(continueCall).toBeDefined();
  expect(continueCall![0]).toEqual({ sessionPk: "s1", prompt: "keep going" });
});

// ---------------------------------------------------------------------------
// SessionTranscript – stop / end buttons
// ---------------------------------------------------------------------------

test("stop button calls stopSession with active sessionPk", async () => {
  useStore.getState().setProjects([{ projectId: "p1", name: "demo", workdir: "/w", harness: "fake", permMode: "default" }]);
  useStore.getState().setSessions([{ sessionPk: "s1", projectId: "p1", title: "fix bug", status: "running" }]);
  useStore.getState().setActive("s1");

  const stopMock = mock(async (_pk: string) => undefined);
  window.harness.stopSession = stopMock;

  const el = await renderInto(<SessionTranscript />);

  const buttons = Array.from(el.querySelectorAll("button"));
  const stopBtn = buttons.find((b) => b.textContent?.trim() === "stop");
  expect(stopBtn).toBeTruthy();

  await act(async () => {
    stopBtn!.click();
  });

  await act(async () => {});

  expect(stopMock.mock.calls.length).toBe(1);
  const stopCall = stopMock.mock.calls[0];
  expect(stopCall).toBeDefined();
  expect(stopCall![0]).toBe("s1");
});

test("end button calls endSession with active sessionPk", async () => {
  useStore.getState().setProjects([{ projectId: "p1", name: "demo", workdir: "/w", harness: "fake", permMode: "default" }]);
  useStore.getState().setSessions([{ sessionPk: "s1", projectId: "p1", title: "fix bug", status: "running" }]);
  useStore.getState().setActive("s1");

  const endMock = mock(async (_pk: string) => undefined);
  window.harness.endSession = endMock;

  const el = await renderInto(<SessionTranscript />);

  const buttons = Array.from(el.querySelectorAll("button"));
  const endBtn = buttons.find((b) => b.textContent?.trim() === "end");
  expect(endBtn).toBeTruthy();

  await act(async () => {
    endBtn!.click();
  });

  await act(async () => {});

  expect(endMock.mock.calls.length).toBe(1);
  const endCall = endMock.mock.calls[0];
  expect(endCall).toBeDefined();
  expect(endCall![0]).toBe("s1");
});
