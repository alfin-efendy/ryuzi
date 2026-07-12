import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

let existsResult = true;
mock.module("@/lib/file-probe", () => ({
  workspaceFileExists: async () => existsResult,
  clearProbeCache: () => {},
}));

const { Markdown } = await import("./Markdown");
const { TranscriptFileContext } = await import("./TranscriptFileContext");
const { useUi } = await import("@/store-ui");

afterEach(cleanup);
beforeEach(() => {
  existsResult = true;
  useUi.setState({ tabs: [], activeTabId: null });
});

const ctx = { runnerId: "local", sessionPk: "s1", workdir: "/home/u/proj" };

test("path-like inline code with a :line suffix opens the file (suffix stripped)", async () => {
  render(
    <TranscriptFileContext.Provider value={ctx}>
      <Markdown text={"see `src/store.ts:42` for details"} />
    </TranscriptFileContext.Provider>,
  );
  const link = await screen.findByRole("button", { name: "src/store.ts:42" });
  fireEvent.click(link);
  expect(useUi.getState().tabs.some((t) => t.path === "/home/u/proj/src/store.ts")).toBe(true);
});

test("non-path inline code and no-context renders stay plain", async () => {
  render(
    <TranscriptFileContext.Provider value={ctx}>
      <Markdown text={"run `cargo test` first"} />
    </TranscriptFileContext.Provider>,
  );
  await Promise.resolve();
  expect(screen.queryByRole("button")).toBeNull();
  cleanup();
  render(<Markdown text={"see `src/store.ts`"} />);
  await Promise.resolve();
  expect(screen.queryByRole("button")).toBeNull();
});

test("windows-style relative paths link once the probe confirms them", async () => {
  render(
    <TranscriptFileContext.Provider value={ctx}>
      <Markdown text={"check `crates\\core\\src\\lib.rs:10:5`"} />
    </TranscriptFileContext.Provider>,
  );
  const link = await screen.findByRole("button", { name: "crates\\core\\src\\lib.rs:10:5" });
  fireEvent.click(link);
  expect(useUi.getState().tabs.some((t) => t.path === "/home/u/proj/crates/core/src/lib.rs")).toBe(true);
});

test("fenced code blocks never linkify", async () => {
  render(
    <TranscriptFileContext.Provider value={ctx}>
      <Markdown text={"```ts\nsrc/store.ts\n```"} />
    </TranscriptFileContext.Provider>,
  );
  await Promise.resolve();
  expect(screen.queryByRole("button")).toBeNull();
});
