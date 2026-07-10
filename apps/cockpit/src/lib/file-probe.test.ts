import { afterEach, beforeEach, expect, mock, test } from "bun:test";

let listDirCalls: Array<[string, string]> = [];
let listDirResult: { status: "ok"; data: { name: string; dir: boolean }[] } | { status: "error"; error: { message: string } } = {
  status: "ok",
  data: [],
};

mock.module("@/bindings", () => ({
  commands: {
    listDir: async (sessionPk: string, rel: string) => {
      listDirCalls.push([sessionPk, rel]);
      return listDirResult;
    },
  },
}));

const { workspaceFileExists, clearProbeCache } = await import("./file-probe");

beforeEach(() => {
  listDirCalls = [];
  listDirResult = { status: "ok", data: [] };
  clearProbeCache();
});
afterEach(() => clearProbeCache());

test("true when the parent listing contains the file entry", async () => {
  listDirResult = {
    status: "ok",
    data: [
      { name: "app.ts", dir: false },
      { name: "sub", dir: true },
    ],
  };
  expect(await workspaceFileExists("s1", "src/app.ts")).toBe(true);
  expect(listDirCalls).toEqual([["s1", "src"]]);
});

test("false for a directory entry of the same name", async () => {
  listDirResult = { status: "ok", data: [{ name: "app.ts", dir: true }] };
  expect(await workspaceFileExists("s1", "src/app.ts")).toBe(false);
});

test("bare basename probes the workdir root", async () => {
  listDirResult = { status: "ok", data: [{ name: "README.md", dir: false }] };
  expect(await workspaceFileExists("s1", "README.md")).toBe(true);
  expect(listDirCalls).toEqual([["s1", ""]]);
});

test("cache: sibling files share one listing; concurrent callers dedup in flight", async () => {
  listDirResult = {
    status: "ok",
    data: [
      { name: "a.ts", dir: false },
      { name: "b.ts", dir: false },
    ],
  };
  const [a, b] = await Promise.all([workspaceFileExists("s1", "src/a.ts"), workspaceFileExists("s1", "src/b.ts")]);
  expect(a).toBe(true);
  expect(b).toBe(true);
  expect(listDirCalls.length).toBe(1);
  // Cached within TTL — still one call.
  expect(await workspaceFileExists("s1", "src/a.ts")).toBe(true);
  expect(listDirCalls.length).toBe(1);
});

test("listDir errors cache as an empty listing (path renders plain)", async () => {
  listDirResult = { status: "error", error: { message: "gone" } };
  expect(await workspaceFileExists("s1", "src/a.ts")).toBe(false);
  expect(await workspaceFileExists("s1", "src/b.ts")).toBe(false);
  expect(listDirCalls.length).toBe(1);
});

test("different sessions do not share cache entries", async () => {
  listDirResult = { status: "ok", data: [{ name: "a.ts", dir: false }] };
  await workspaceFileExists("s1", "src/a.ts");
  await workspaceFileExists("s2", "src/a.ts");
  expect(listDirCalls.length).toBe(2);
});
