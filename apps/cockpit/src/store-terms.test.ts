import { test, expect } from "bun:test";
import { nextTitle, closeTermTab, markExited, type TermTab } from "./store-terms";

const tab = (id: string, title: string, exited = false): TermTab => ({ termId: id, title, exited });

test("nextTitle numbers past the highest existing terminal", () => {
  expect(nextTitle([])).toBe("Terminal 1");
  expect(nextTitle([tab("a", "Terminal 1"), tab("b", "Terminal 3")])).toBe("Terminal 4");
});

test("closeTermTab focuses a neighbor like the file dock", () => {
  const tabs = [tab("a", "Terminal 1"), tab("b", "Terminal 2"), tab("c", "Terminal 3")];
  const r = closeTermTab(tabs, "b", "b");
  expect(r.tabs.map((t) => t.termId)).toEqual(["a", "c"]);
  expect(r.active).toBe("c");
  const last = closeTermTab(r.tabs, "c", "c");
  expect(last.active).toBe("a");
  expect(closeTermTab([tab("a", "Terminal 1")], "a", "a").active).toBeNull();
});

test("closeTermTab keeps focus when closing an inactive tab", () => {
  const r = closeTermTab([tab("a", "Terminal 1"), tab("b", "Terminal 2")], "a", "b");
  expect(r.active).toBe("a");
});

test("markExited flags the tab without removing it", () => {
  const r = markExited([tab("a", "Terminal 1")], "a");
  expect(r[0].exited).toBe(true);
});
