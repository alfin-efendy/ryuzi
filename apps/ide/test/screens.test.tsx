// apps/ide/test/screens.test.tsx
import { test, expect, mock, beforeAll, afterAll } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";
beforeAll(() => {
  if (!globalThis.document) GlobalRegistrator.register();
  globalThis.IS_REACT_ACT_ENVIRONMENT = true;
});
afterAll(() => {
  GlobalRegistrator.unregister();
});

import React from "react";
import { act } from "react";
import { createRoot } from "react-dom/client";
import { useStore } from "../src/renderer/store";
import { ProjectsSessionsTree } from "../src/renderer/screens/ProjectsSessionsTree";

test("tree renders projects and their sessions with status", async () => {
  useStore.getState().setProjects([{ projectId: "p1", name: "demo", workdir: "/w", harness: "fake", permMode: "default" }]);
  useStore.getState().setSessions([{ sessionPk: "s1", projectId: "p1", title: "fix bug", status: "running" }]);
  const el = document.createElement("div");
  const root = createRoot(el);
  await act(async () => {
    root.render(<ProjectsSessionsTree />);
  });
  expect(el.textContent).toContain("demo");
  expect(el.textContent).toContain("fix bug");
});
