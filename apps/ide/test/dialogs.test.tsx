// apps/ide/test/dialogs.test.tsx
import { test, expect, mock, beforeAll, afterAll, beforeEach } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
beforeAll(() => {
  if (!globalThis.document) {
    GlobalRegistrator.register();
    (globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;
  }
});
afterAll(() => GlobalRegistrator.unregister());

// happy-dom does not support Radix Portal (ReactDOM.createPortal renders to document.body but
// @radix-ui/react-dialog's internal portal + presence machinery never flushes its state in this
// environment).  We stub the shadcn Dialog wrappers with simple inline renderers so the form
// content is accessible in the container.  Production dialog.tsx keeps the real Portal+Overlay —
// this mock is TEST-SCOPE only.
mock.module("@/components/ui/dialog", () => {
  const React = require("react");

  function Dialog({ children, open, onOpenChange }: any) {
    // Pass open/onOpenChange down via context so DialogTrigger can toggle
    return React.createElement(DialogCtx.Provider, { value: { open: !!open, onOpenChange: onOpenChange ?? (() => {}) } }, children);
  }
  const DialogCtx = React.createContext({ open: false, onOpenChange: (_v: boolean) => {} });

  function DialogTrigger({ children, asChild }: any) {
    const ctx = React.useContext(DialogCtx);
    const child = asChild && React.Children.only(children);
    if (child) return React.cloneElement(child, { onClick: () => ctx.onOpenChange(true) });
    return React.createElement("button", { onClick: () => ctx.onOpenChange(true) }, children);
  }

  const DialogContent = React.forwardRef(({ children }: any, _ref: any) => React.createElement("div", { role: "dialog" }, children));
  DialogContent.displayName = "DialogContent";

  const DialogHeader = ({ children }: any) => React.createElement("div", null, children);
  DialogHeader.displayName = "DialogHeader";

  const DialogTitle = ({ children }: any) => React.createElement("h2", null, children);
  DialogTitle.displayName = "DialogTitle";

  const DialogFooter = ({ children }: any) => React.createElement("div", null, children);
  DialogFooter.displayName = "DialogFooter";

  const DialogDescription = ({ children }: any) => React.createElement("p", null, children);
  DialogDescription.displayName = "DialogDescription";

  const DialogClose = ({ children }: any) => React.createElement(React.Fragment, null, children);
  DialogClose.displayName = "DialogClose";

  const DialogPortal = ({ children }: any) => React.createElement(React.Fragment, null, children);
  DialogPortal.displayName = "DialogPortal";

  const DialogOverlay = React.forwardRef((_props: any, _ref: any) => null);
  DialogOverlay.displayName = "DialogOverlay";

  return {
    Dialog,
    DialogTrigger,
    DialogContent,
    DialogHeader,
    DialogTitle,
    DialogFooter,
    DialogDescription,
    DialogClose,
    DialogPortal,
    DialogOverlay,
  };
});

import React from "react";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { useStore } from "../src/renderer/store";
import { ConnectProjectDialog } from "../src/renderer/screens/ConnectProjectDialog";
import { NewSessionDialog } from "../src/renderer/screens/NewSessionDialog";

function props(el: Element) {
  const k = Object.keys(el).find((x) => x.startsWith("__reactProps$"))!;
  return (el as any)[k];
}

beforeEach(() => {
  useStore.setState({ projects: [], sessions: [], activeSessionPk: null, connId: "conn-1" });
  (window as any).harness = {
    connectProject: mock(async (input: any) => ({
      projectId: "p1",
      name: "y",
      workdir: "/w",
      harness: "fake",
      permMode: "default",
      ...input,
    })),
    listProjects: mock(async () => [{ projectId: "p1", name: "y", workdir: "/w", harness: "fake", permMode: "default" }]),
    startSession: mock(async (_input: any) => ({ sessionPk: "sess-1" })),
    listSessions: mock(async () => []),
  };
});

test("NewSessionDialog shows inline error and stays open when startSession rejects", async () => {
  (window as any).harness.startSession = mock(async () => {
    throw new Error("gateway unavailable");
  });

  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);

  // Render with dialog open by wrapping in a controlled parent
  await act(async () => {
    root.render(<NewSessionDialog projectId="p1" />);
  });

  // Open the dialog via the trigger button
  const trigger = [...document.body.querySelectorAll("button")].find((b) => /new session/i.test(b.textContent ?? ""))!;
  await act(async () => {
    props(trigger).onClick({ preventDefault() {} });
  });

  // Type a prompt
  const input = document.body.querySelector("input")!;
  await act(async () => {
    props(input).onChange({ target: { value: "do something" } });
  });

  // Click Start
  const startBtn = [...document.body.querySelectorAll("button")].find((b) => /start/i.test(b.textContent ?? ""))!;
  await act(async () => {
    props(startBtn).onClick({ preventDefault() {} });
  });

  // Error message should be visible
  const errorEl = document.body.querySelector("p.text-xs");
  expect(errorEl?.textContent).toBe("gateway unavailable");

  // Dialog should still be open — the input is still present
  expect(document.body.querySelector("input")).not.toBeNull();

  // setActive should NOT have been called
  expect(useStore.getState().activeSessionPk).toBeNull();

  await act(async () => {
    root.unmount();
  });
  container.remove();
});

test("ConnectProjectDialog submits gitUrl and refreshes projects", async () => {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  // Render with the dialog forced open for testability.
  await act(async () => {
    root.render(<ConnectProjectDialog defaultOpen />);
  });
  // Dialog is stubbed inline (no portal), so content is inside the container.
  const input = document.body.querySelector("input")!;
  await act(async () => {
    props(input).onChange({ target: { value: "https://x/y.git" } });
  });
  const submit = [...document.body.querySelectorAll("button")].find((b) => /connect/i.test(b.textContent ?? ""))!;
  await act(async () => {
    props(submit).onClick({ preventDefault() {} });
  });
  expect((window as any).harness.connectProject).toHaveBeenCalledWith({ gitUrl: "https://x/y.git" });
  expect((window as any).harness.listProjects).toHaveBeenCalled();
  expect(useStore.getState().projects.length).toBeGreaterThan(0);
  await act(async () => {
    root.unmount();
  });
  container.remove();
});
