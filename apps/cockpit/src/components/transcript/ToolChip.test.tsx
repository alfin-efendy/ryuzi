import { afterEach, beforeEach, expect, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ActivityItem } from "@/lib/transcript";
import { TranscriptFileContext } from "./TranscriptFileContext";
import { useUi } from "@/store-ui";

const { ActivityCluster } = await import("./ToolChip");

afterEach(cleanup);
beforeEach(() => {
  useUi.setState({ tabs: [], activeTabId: null });
});

function toolItem(key: string, status = "completed"): ActivityItem {
  return {
    type: "tool",
    key,
    name: "read",
    kind: "read",
    status,
    output: null,
    path: null,
    input: { file_path: `src/${key}.ts` },
    durationMs: null,
    exitCode: null,
    summary: null,
  };
}

test("fold=false renders every item flat (no See N steps)", () => {
  render(<ActivityCluster items={[toolItem("a"), toolItem("b")]} />);
  expect(screen.queryByText(/See \d+ step/)).toBeNull();
});

test("fold with liveTail shows the tail and a See N steps row counting the whole run", () => {
  const items = ["a", "b", "c", "d", "e"].map((k) => toolItem(k));
  render(<ActivityCluster items={items} fold liveTail />);
  expect(screen.getByText("See 5 steps")).toBeTruthy();
});

test("expanding the fold reveals the hidden chips", () => {
  const items = ["a", "b", "c", "d", "e"].map((k) => toolItem(k));
  render(<ActivityCluster items={items} fold liveTail />);
  // Folded: the two oldest (a, b) are hidden.
  expect(screen.queryByText((c) => c.includes("src/a.ts"))).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: /See 5 steps/ }));
  expect(screen.getByText((c) => c.includes("src/a.ts"))).toBeTruthy();
});

test("fold without liveTail folds the entire cluster", () => {
  const items = ["a", "b"].map((k) => toolItem(k));
  render(<ActivityCluster items={items} fold />);
  expect(screen.getByText("See 2 steps")).toBeTruthy();
  expect(screen.queryByText("src/a.ts")).toBeNull();
});

test("singular label for a one-step fold", () => {
  render(<ActivityCluster items={[toolItem("a")]} fold />);
  expect(screen.getByText("See 1 step")).toBeTruthy();
});

test("a tool card's path opens the file without toggling the card", async () => {
  const item = {
    ...toolItem("t1"),
    path: "src/a.ts",
    output: "file contents",
  };
  render(
    <TranscriptFileContext.Provider value={{ sessionPk: "s1", workdir: "/home/u/proj" }}>
      <ActivityCluster items={[item]} />
    </TranscriptFileContext.Provider>,
  );
  const link = screen.getByRole("link", { name: /src\/a\.ts/ });
  fireEvent.click(link);
  expect(useUi.getState().tabs.some((t) => t.path === "/home/u/proj/src/a.ts")).toBe(true);
  // stopPropagation: the expandable card did not toggle open.
  expect(screen.queryByText("file contents")).toBeNull();
});

test("without a provider the detail stays a plain span", () => {
  const item = { ...toolItem("t1"), path: "src/a.ts" };
  render(<ActivityCluster items={[item]} />);
  expect(screen.queryByRole("link")).toBeNull();
});
