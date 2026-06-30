import { test, expect } from "bun:test";
import { openFileTab, closeTab, normalizeActive, type DockTab } from "./store-ui";

const fileTab = (path: string): DockTab => ({ id: path, kind: "file", path, title: path.split("/").pop() ?? path });

test("openFileTab appends a new tab and focuses it", () => {
  const r = openFileTab([], "/a/b.ts");
  expect(r.tabs).toHaveLength(1);
  expect(r.tabs[0]).toEqual({ id: "/a/b.ts", kind: "file", path: "/a/b.ts", title: "b.ts" });
  expect(r.activeTabId).toBe("/a/b.ts");
});

test("openFileTab dedupes by path and just refocuses", () => {
  const start = [fileTab("/a/b.ts"), fileTab("/c/d.ts")];
  const r = openFileTab(start, "/a/b.ts");
  expect(r.tabs).toHaveLength(2);
  expect(r.activeTabId).toBe("/a/b.ts");
});

test("closeTab on active focuses the right neighbor", () => {
  const start = [fileTab("/a.ts"), fileTab("/b.ts"), fileTab("/c.ts")];
  const r = closeTab(start, "/b.ts", "/b.ts");
  expect(r.tabs.map((t) => t.id)).toEqual(["/a.ts", "/c.ts"]);
  expect(r.activeTabId).toBe("/c.ts");
});

test("closeTab on last active focuses the left neighbor", () => {
  const start = [fileTab("/a.ts"), fileTab("/b.ts")];
  const r = closeTab(start, "/b.ts", "/b.ts");
  expect(r.activeTabId).toBe("/a.ts");
});

test("closeTab on the only tab yields null active", () => {
  const r = closeTab([fileTab("/a.ts")], "/a.ts", "/a.ts");
  expect(r.tabs).toHaveLength(0);
  expect(r.activeTabId).toBeNull();
});

test("closeTab on a non-active tab keeps the active focus", () => {
  const start = [fileTab("/a.ts"), fileTab("/b.ts")];
  const r = closeTab(start, "/a.ts", "/b.ts");
  expect(r.activeTabId).toBe("/a.ts");
});

test("normalizeActive maps empty/null to null, keeps real ids", () => {
  expect(normalizeActive("")).toBeNull();
  expect(normalizeActive(null)).toBeNull();
  expect(normalizeActive("/a.ts")).toBe("/a.ts");
});
