import { describe, expect, test } from "bun:test";
import { basename, fileBadge } from "./paths";

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
