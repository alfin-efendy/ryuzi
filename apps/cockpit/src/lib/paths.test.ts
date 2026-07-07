import { describe, expect, test } from "bun:test";
import { basename, fileBadge } from "./paths";
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
