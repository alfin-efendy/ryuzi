import { test, expect } from "bun:test";
import { resolveToolPolicy, canApprove, summarizeTool, isAdmin, gatePermMode, parseRoleIds } from "../src/core/permissions";

test("resolveToolPolicy by mode and tool", () => {
  expect(resolveToolPolicy("bypassPermissions", "Bash")).toBe("allow");
  expect(resolveToolPolicy("default", "Read")).toBe("allow");
  expect(resolveToolPolicy("default", "Bash")).toBe("ask");
  expect(resolveToolPolicy("default", "Edit")).toBe("ask");
  expect(resolveToolPolicy("acceptEdits", "Edit")).toBe("allow");
  expect(resolveToolPolicy("acceptEdits", "Bash")).toBe("ask");
});

test("canApprove gating", () => {
  expect(canApprove({ clickerRoleIds: [], approverRoleIds: [], isStarter: true })).toBe(true);
  expect(canApprove({ clickerRoleIds: [], approverRoleIds: [], isStarter: false })).toBe(false); // none configured => only starter
  expect(canApprove({ clickerRoleIds: ["r1"], approverRoleIds: ["r1"], isStarter: false })).toBe(true);
  expect(canApprove({ clickerRoleIds: ["r2"], approverRoleIds: ["r1"], isStarter: false })).toBe(false);
});

test("summarizeTool", () => {
  expect(summarizeTool("Bash", { command: "echo hi" })).toBe("Bash: echo hi");
  expect(summarizeTool("Edit", { file_path: "src/a.ts" })).toBe("Edit: src/a.ts");
  expect(summarizeTool("Glob", {})).toBe("Glob");
});

test("parseRoleIds splits, trims, drops empties", () => {
  expect(parseRoleIds("a, b ,,c")).toEqual(["a", "b", "c"]);
  expect(parseRoleIds("")).toEqual([]);
  expect(parseRoleIds(undefined)).toEqual([]);
});

test("isAdmin: blank admin roles => everyone is admin", () => {
  expect(isAdmin({ userRoleIds: [], adminRoleIds: [] })).toBe(true);
  expect(isAdmin({ userRoleIds: ["x"], adminRoleIds: [] })).toBe(true);
});

test("isAdmin: configured admin roles gate membership", () => {
  expect(isAdmin({ userRoleIds: ["admin"], adminRoleIds: ["admin"] })).toBe(true);
  expect(isAdmin({ userRoleIds: ["other"], adminRoleIds: ["admin"] })).toBe(false);
  expect(isAdmin({ userRoleIds: [], adminRoleIds: ["admin"] })).toBe(false);
});

test("gatePermMode: only non-admin bypassPermissions is clamped", () => {
  expect(gatePermMode("bypassPermissions", false)).toEqual({ mode: "default", downgraded: true });
  expect(gatePermMode("bypassPermissions", true)).toEqual({ mode: "bypassPermissions", downgraded: false });
  expect(gatePermMode("acceptEdits", false)).toEqual({ mode: "acceptEdits", downgraded: false });
  expect(gatePermMode("default", false)).toEqual({ mode: "default", downgraded: false });
});
