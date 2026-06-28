// apps/ide/test/connections-dialog.test.tsx
import { test, expect, mock, beforeAll, afterAll, beforeEach } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

// Stub the shadcn dialog at test-scope so Radix portals don't block happy-dom (2b pattern).
mock.module("@/components/ui/dialog", () => {
  const React = require("react");
  const Dialog = ({ children }: any) => React.createElement("div", null, children);
  const pass = ({ children }: any) => React.createElement("div", null, children);
  return {
    Dialog,
    DialogTrigger: pass,
    DialogContent: pass,
    DialogHeader: pass,
    DialogTitle: pass,
    DialogPortal: pass,
    DialogOverlay: pass,
    DialogClose: pass,
    DialogDescription: pass,
    DialogFooter: pass,
  };
});

beforeAll(() => {
  if (!globalThis.document) {
    GlobalRegistrator.register();
    (globalThis as any).IS_REACT_ACT_ENVIRONMENT = true;
  }
});
afterAll(() => GlobalRegistrator.unregister());

import React from "react";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { useStore } from "../src/renderer/store";
import { ConnectionsDialog } from "../src/renderer/screens/ConnectionsDialog";

function props(el: Element) {
  const k = Object.keys(el).find((x) => x.startsWith("__reactProps$"))!;
  return (el as any)[k];
}

beforeEach(() => {
  useStore.setState({
    connections: [
      { id: "local", label: "Local (hr serve)", baseUrl: "http://127.0.0.1:8787", authMode: "loopback", active: true, signedIn: true },
      { id: "p2", label: "Cloud", baseUrl: "https://r", authMode: "oidc", active: false, signedIn: false },
    ],
  });
  (window as any).harness = {
    selectConnection: mock(async () => {}),
    signIn: mock(async () => {}),
    signOut: mock(async () => {}),
    addConnection: mock(async () => {}),
    removeConnection: mock(async () => {}),
    listConnections: mock(async () => []),
  };
});

test("renders connections; Select on the cloud profile calls selectConnection", async () => {
  const el = document.createElement("div");
  const root = createRoot(el);
  await act(async () => {
    root.render(<ConnectionsDialog defaultOpen />);
  });
  expect(el.textContent).toContain("Cloud");
  const select = [...el.querySelectorAll("button")].find((b) => /select/i.test(b.textContent ?? ""))!;
  await act(async () => {
    props(select).onClick({});
  });
  expect((window as any).harness.selectConnection).toHaveBeenCalledWith("p2");
  await act(async () => {
    root.unmount();
  });
});
