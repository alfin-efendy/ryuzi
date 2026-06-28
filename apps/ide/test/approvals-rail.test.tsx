// apps/ide/test/approvals-rail.test.tsx
// Migrated from ApprovalsRail (deleted in T8) to ApprovalCard inline component.
// The card component is now used directly in SessionTranscript; this file tests
// the ApprovalCard in isolation to preserve the original coverage intent.
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
import { ApprovalCard } from "../src/renderer/screens/ApprovalCard";

let container: HTMLElement;
let root: Root;

beforeEach(() => {
  useStore.setState({
    activeSessionPk: "s1",
    connId: null,
    projects: [],
    transcripts: {},
    pendingApprovals: [{ t: "approval.request", requestId: "r1", sessionPk: "s1", tool: "Bash", summary: "Bash: ls", timeoutMs: 60000 }],
    openFilePath: null,
    openFile: null,
  });
  (window as any).harness = { resolveApproval: mock(() => {}) };
  container = document.createElement("div");
  document.body.appendChild(container);
});

afterEach(async () => {
  if (root) {
    await act(async () => {
      root.unmount();
    });
  }
  container.remove();
  useStore.setState({
    activeSessionPk: null,
    pendingApprovals: [],
    transcripts: {},
    openFilePath: null,
    openFile: null,
  });
  (window as any).harness = undefined;
});

test("renders an approval card and Allow calls resolveApproval + removes it", async () => {
  const req = useStore.getState().pendingApprovals[0]!;
  root = createRoot(container);
  await act(async () => {
    root.render(<ApprovalCard req={req} />);
  });
  expect(container.textContent).toContain("Bash: ls");
  const allow = [...container.querySelectorAll("button")].find((b) => /allow/i.test(b.textContent ?? ""))!;
  expect(allow).toBeTruthy();
  await act(async () => {
    (allow as any)[Object.keys(allow).find((k) => k.startsWith("__reactProps$"))!].onClick({});
  });
  expect((window as any).harness.resolveApproval).toHaveBeenCalledWith("r1", "allow");
  expect(useStore.getState().pendingApprovals.length).toBe(0);
});
