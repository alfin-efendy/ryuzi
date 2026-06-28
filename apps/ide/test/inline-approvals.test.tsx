// apps/ide/test/inline-approvals.test.tsx
import { test, expect, mock, beforeAll, afterAll, beforeEach, afterEach } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
beforeAll(() => {
  if (!globalThis.document) {
    GlobalRegistrator.register();
    (globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;
  }
});
afterAll(() => GlobalRegistrator.unregister());

import React from "react";
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { useStore } from "../src/renderer/store";
import { SessionTranscript } from "../src/renderer/screens/SessionTranscript";

function props(el: Element) {
  const k = Object.keys(el).find((x) => x.startsWith("__reactProps$"))!;
  return (el as any)[k];
}

const roots: Root[] = [];
const containers: HTMLElement[] = [];

beforeEach(() => {
  useStore.setState({
    activeSessionPk: "s1",
    connId: "c1",
    projects: [],
    transcripts: { s1: [] },
    pendingApprovals: [{ t: "approval.request", requestId: "r1", sessionPk: "s1", tool: "bash", summary: "rm -rf", timeoutMs: 30000 }],
  });
  (window as any).harness = {
    resolveApproval: mock(() => {}),
    continueSession: mock(async () => {}),
    stopSession: mock(async () => {}),
    endSession: mock(async () => {}),
  };
});

afterEach(async () => {
  for (const root of roots) {
    await act(async () => {
      root.unmount();
    });
  }
  for (const container of containers) {
    container.remove();
  }
  roots.length = 0;
  containers.length = 0;
  useStore.setState({
    activeSessionPk: null,
    pendingApprovals: [],
    transcripts: {},
    openFilePath: null,
    openFile: null,
  });
  (window as any).harness = undefined;
});

test("a pending approval for the active session renders inline; Allow resolves it", async () => {
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  roots.push(root);
  containers.push(el);
  await act(async () => {
    root.render(<SessionTranscript />);
  });
  expect(el.textContent).toContain("bash");
  const allow = [...el.querySelectorAll("button")].find((b) => /allow/i.test(b.textContent ?? ""))!;
  expect(allow).toBeTruthy();
  await act(async () => {
    props(allow).onClick({});
  });
  expect((window as any).harness.resolveApproval).toHaveBeenCalledWith("r1", "allow");
  expect(useStore.getState().pendingApprovals).toHaveLength(0);
});

test("a pending approval for a DIFFERENT session does NOT render inline", async () => {
  useStore.setState({
    pendingApprovals: [
      { t: "approval.request", requestId: "r2", sessionPk: "s2", tool: "read", summary: "read /etc/passwd", timeoutMs: 30000 },
    ],
  });
  const el = document.createElement("div");
  document.body.appendChild(el);
  const root = createRoot(el);
  roots.push(root);
  containers.push(el);
  await act(async () => {
    root.render(<SessionTranscript />);
  });
  // The approval is for s2 but active session is s1, so it should NOT render
  const allow = [...el.querySelectorAll("button")].find((b) => /allow/i.test(b.textContent ?? ""));
  expect(allow).toBeUndefined();
});
