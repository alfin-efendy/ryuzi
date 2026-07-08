import { test, expect } from "bun:test";
import { openFileTab, closeTab, normalizeActive, setTabMode, type DockTab } from "./store-ui";

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

test("setTabMode sets mode on the target tab only", () => {
  const start = [fileTab("/a/readme.md"), fileTab("/c/d.ts")];
  const r = setTabMode(start, "/a/readme.md", "code");
  expect(r.find((t) => t.id === "/a/readme.md")?.mode).toBe("code");
  expect(r.find((t) => t.id === "/c/d.ts")?.mode).toBeUndefined();
});

test("setTabMode with an unknown id leaves tabs unchanged", () => {
  const start = [fileTab("/a.ts")];
  expect(setTabMode(start, "/nope.ts", "view")).toEqual(start);
});

test("tabs persisted before the mode field stay valid and can adopt a mode", () => {
  // Exact shape old clients wrote to localStorage (no mode key).
  const legacy = JSON.parse('[{"id":"/a.md","kind":"file","path":"/a.md","title":"a.md"}]') as DockTab[];
  expect(legacy[0].mode).toBeUndefined();
  expect(setTabMode(legacy, "/a.md", "view")[0].mode).toBe("view");
});
