import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, render, screen, waitFor, fireEvent } from "@testing-library/react";

let existsResult = true;
mock.module("@/lib/file-probe", () => ({
  workspaceFileExists: async () => existsResult,
  clearProbeCache: () => {},
}));

const { TranscriptFileContext, WorkspacePathCode } = await import("./TranscriptFileContext");
const { useUi } = await import("@/store-ui");
const { useNav } = await import("@/store-nav");

afterEach(cleanup);
beforeEach(() => {
  existsResult = true;
});

const ctx = { runnerId: "local", sessionPk: "s1", workdir: "/home/u/proj" };

function renderInCtx(text: string) {
  return render(
    <TranscriptFileContext.Provider value={ctx}>
      <WorkspacePathCode text={text} />
    </TranscriptFileContext.Provider>,
  );
}

test("outside a provider renders a plain code span", () => {
  render(<WorkspacePathCode text="src/app.ts" />);
  expect(screen.queryByRole("button")).toBeNull();
  expect(screen.getByText("src/app.ts").tagName).toBe("CODE");
});

test("non-path text never links", async () => {
  renderInCtx("npm install");
  await Promise.resolve();
  expect(screen.queryByRole("button")).toBeNull();
});

test("an existing path becomes a clickable span that opens the file tab", async () => {
  renderInCtx("src/app.ts");
  const link = await screen.findByRole("button", { name: "src/app.ts" });
  fireEvent.click(link);
  const tabs = useUi.getState().tabs;
  expect(tabs.some((t) => t.path === "/home/u/proj/src/app.ts")).toBe(true);
  expect(useNav.getState().rightOpen).toBe(true);
  expect(useNav.getState().rightTab).toBe("file");
});

test("a non-existing path stays plain", async () => {
  existsResult = false;
  renderInCtx("src/ghost.ts");
  await waitFor(() => expect(screen.queryByRole("button")).toBeNull());
  expect(screen.getByText("src/ghost.ts")).toBeTruthy();
});

test("a backslash-relative path fails the shape gate and stays plain even when it exists", async () => {
  renderInCtx("a\\b");
  await Promise.resolve();
  await Promise.resolve();
  expect(screen.queryByRole("button")).toBeNull();
  expect(screen.getByText("a\\b").tagName).toBe("CODE");
});

test("an absolute path under the workdir is trusted and links", async () => {
  renderInCtx("/home/u/proj/src/app.ts");
  const link = await screen.findByRole("button", { name: "/home/u/proj/src/app.ts" });
  fireEvent.click(link);
  const tabs = useUi.getState().tabs;
  expect(tabs.some((t) => t.path === "/home/u/proj/src/app.ts")).toBe(true);
});
