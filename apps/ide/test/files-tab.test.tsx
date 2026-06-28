// apps/ide/test/files-tab.test.tsx
import { test, expect, mock, beforeAll, afterAll, beforeEach, afterEach } from "bun:test";
import { GlobalRegistrator } from "@happy-dom/global-registrator";

// CodeMirror is DOM/worker-heavy; stub it (2b pattern) and assert the props we pass.
mock.module("@uiw/react-codemirror", () => {
  const React = require("react");
  return {
    default: (props: any) =>
      React.createElement(
        "pre",
        { "data-testid": "cm", "data-readonly": String(props.readOnly), "data-editable": String(props.editable) },
        props.value,
      ),
  };
});
mock.module("@uiw/codemirror-extensions-langs", () => ({ loadLanguage: () => null }));

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
import { FilesTab } from "../src/renderer/screens/FilesTab";

function props(el: Element) {
  const k = Object.keys(el).find((x) => x.startsWith("__reactProps$"))!;
  return (el as any)[k];
}

const roots: ReturnType<typeof createRoot>[] = [];

beforeEach(() => {
  useStore.setState({ activeSessionPk: "s1", openFilePath: null, openFile: null });
  (window as any).harness = {
    listDir: mock(async (_req: any) => [
      { name: "src", type: "dir" },
      { name: "README.md", type: "file" },
    ]),
    readFile: mock(async (_req: any) => ({ content: "# hi", encoding: "utf8", binary: false, truncated: false })),
  };
});

afterEach(async () => {
  await act(async () => {
    for (const root of roots) {
      root.unmount();
    }
    roots.length = 0;
  });
  useStore.setState({ activeSessionPk: null, openFilePath: null, openFile: null });
  (window as any).harness = undefined;
});

test("renders the worktree root tree and opens a file on click", async () => {
  const el = document.createElement("div");
  const root = createRoot(el);
  roots.push(root);
  await act(async () => {
    root.render(<FilesTab />);
  });
  await act(async () => {}); // flush the listDir effect
  expect((window as any).harness.listDir).toHaveBeenCalledWith({ sessionPk: "s1", path: "" });
  expect(el.textContent).toContain("README.md");
  const fileBtn = [...el.querySelectorAll("button")].find((b) => /README\.md/.test(b.textContent ?? ""))!;
  await act(async () => {
    props(fileBtn).onClick({});
    await Promise.resolve(); // ensure the readFile promise resolves inside act
  });
  await act(async () => {}); // flush any remaining microtasks/state updates
  expect((window as any).harness.readFile).toHaveBeenCalledWith({ sessionPk: "s1", path: "README.md" });
  const cmEl = el.querySelector("[data-testid=cm]")!;
  expect(cmEl.textContent).toBe("# hi");
  expect(cmEl.getAttribute("data-readonly")).toBe("true");
  expect(cmEl.getAttribute("data-editable")).toBe("false");
});

test("empty state when no active session", async () => {
  useStore.setState({ activeSessionPk: null });
  const el = document.createElement("div");
  const root = createRoot(el);
  roots.push(root);
  await act(async () => {
    root.render(<FilesTab />);
  });
  expect(el.textContent?.toLowerCase()).toContain("session");
});
