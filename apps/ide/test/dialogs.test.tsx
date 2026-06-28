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

import React from "react";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { useStore } from "../src/renderer/store";
import { ConnectProjectDialog } from "../src/renderer/screens/ConnectProjectDialog";

function props(el: Element) {
  const k = Object.keys(el).find((x) => x.startsWith("__reactProps$"))!;
  return (el as any)[k];
}

beforeEach(() => {
  useStore.setState({ projects: [] });
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
  };
});

test("ConnectProjectDialog submits gitUrl and refreshes projects", async () => {
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  // Render with the dialog forced open for testability.
  await act(async () => {
    root.render(<ConnectProjectDialog defaultOpen />);
  });
  // Dialog content renders into a portal in document.body, so query there
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
  await act(async () => {
    root.unmount();
  });
  container.remove();
});
