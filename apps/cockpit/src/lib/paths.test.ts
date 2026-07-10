import { describe, expect, test } from "bun:test";
import { basename, fileBadge, parsePathToken } from "./paths";
import { joinPath } from "./paths";
import { toRepoRelative } from "./paths";

describe("basename", () => {
  test("posix paths", () => {
    expect(basename("/a/b/file.ts")).toBe("file.ts");
    expect(basename("/a/b/")).toBe("b");
  });
  test("windows paths", () => {
    expect(basename("C:\\repo\\x.ts")).toBe("x.ts");
    expect(basename("C:\\repo\\dir\\")).toBe("dir");
  });
  test("bare name and empty", () => {
    expect(basename("Makefile")).toBe("Makefile");
    expect(basename("")).toBe("");
  });
});

describe("fileBadge", () => {
  test("known extensions", () => {
    expect(fileBadge("C:\\repo\\x.rs")).toBe("RS");
    expect(fileBadge("/a/b.tsx")).toBe("TS");
    expect(fileBadge("/a/b.json")).toBe("{}");
  });
  test("unknown extension truncates to 4 chars uppercased", () => {
    expect(fileBadge("/a/b.customext")).toBe("CUST");
  });
  test("no extension", () => {
    expect(fileBadge("/a/Makefile")).toBe("FILE");
  });
});

test("joinPath uses the workdir's separator", () => {
  expect(joinPath("C:\\work\\proj", "src/lib/a.ts")).toBe("C:\\work\\proj\\src\\lib\\a.ts");
  expect(joinPath("/home/u/proj", "src/lib/a.ts")).toBe("/home/u/proj/src/lib/a.ts");
});

test("joinPath tolerates trailing separators and empty segments", () => {
  expect(joinPath("C:\\work\\proj\\", "a.ts")).toBe("C:\\work\\proj\\a.ts");
  expect(joinPath("/home/u/proj/", "dir//b.ts")).toBe("/home/u/proj/dir/b.ts");
});

test("toRepoRelative strips the workdir prefix across separators", () => {
  expect(toRepoRelative("C:\\work\\proj\\src\\a.ts", "C:\\work\\proj")).toBe("src/a.ts");
  expect(toRepoRelative("/home/u/proj/src/a.ts", "/home/u/proj")).toBe("src/a.ts");
  expect(toRepoRelative("src/a.ts", "C:\\work\\proj")).toBe("src/a.ts");
});

// Tests for looksLikeWorkspaceFilePath and toWorkspaceRelativePath
import { looksLikeWorkspaceFilePath, toWorkspaceRelativePath } from "./paths";

const W = "/home/u/project";

test.each([
  ["src/app.ts", true],
  ["docs/Design Notes.md", true], // spaces inside a segment are fine
  ["a/b/c.rs", true],
  ["/etc/passwd", false], // absolute
  ["https://x.dev/a/b", false], // URL
  ["src/app.ts?raw", false], // query
  ["src/app.ts#L10", false], // fragment
  ["app.ts", false], // no parent segment
  ["git diff src/app.ts", false], // whitespace before first slash
  ["src//app.ts", false], // empty segment
  ["src/../secret", false], // dot-dot
  ["./src/app.ts", false], // dot segment
  ["", false],
])("looksLikeWorkspaceFilePath(%p) === %p", (input, expected) => {
  expect(looksLikeWorkspaceFilePath(input)).toBe(expected);
});

test("relative path with safe segments passes through", () => {
  expect(toWorkspaceRelativePath("src/app.ts", W)).toBe("src/app.ts");
});

test("absolute path under the workdir is stripped to relative", () => {
  expect(toWorkspaceRelativePath(`${W}/src/app.ts`, W)).toBe("src/app.ts");
});

test("absolute path outside the workdir is rejected", () => {
  expect(toWorkspaceRelativePath("/etc/passwd", W)).toBeNull();
});

test("the workdir itself is rejected", () => {
  expect(toWorkspaceRelativePath(W, W)).toBeNull();
});

test("url/query/fragment rejected before any resolution", () => {
  expect(toWorkspaceRelativePath("https://x.dev/a", W)).toBeNull();
  expect(toWorkspaceRelativePath(`${W}/a.ts#L1`, W)).toBeNull();
  expect(toWorkspaceRelativePath(`${W}/a.ts?x`, W)).toBeNull();
});

test("unsafe segments rejected after stripping", () => {
  expect(toWorkspaceRelativePath(`${W}/src/../x.ts`, W)).toBeNull();
  expect(toWorkspaceRelativePath("src/../x.ts", W)).toBeNull();
});

test("windows-style workdir separators are tolerated", () => {
  expect(toWorkspaceRelativePath("C:\\w\\proj\\src\\app.ts", "C:\\w\\proj")).toBe("src/app.ts");
});

test("parsePathToken accepts relative and absolute paths with optional :line[:col]", () => {
  expect(parsePathToken("src/store.ts")).toEqual({ path: "src/store.ts", line: null });
  expect(parsePathToken("src/store.ts:42")).toEqual({ path: "src/store.ts", line: 42 });
  expect(parsePathToken(String.raw`crates\core\src\lib.rs:10:5`)).toEqual({ path: String.raw`crates\core\src\lib.rs`, line: 10 });
  expect(parsePathToken(String.raw`C:\work\proj\src\a.ts:7`)).toEqual({ path: String.raw`C:\work\proj\src\a.ts`, line: 7 });
  expect(parsePathToken("src/.env")).toEqual({ path: "src/.env", line: null });
});

test("parsePathToken rejects non-paths", () => {
  expect(parsePathToken("and/or")).toBeNull();
  expect(parsePathToken("store.ts")).toBeNull();
  expect(parsePathToken("https://example.com/a.ts")).toBeNull();
  expect(parsePathToken("run this/that.ts now")).toBeNull();
  expect(parsePathToken("cargo test")).toBeNull();
});
