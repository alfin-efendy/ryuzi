import { expect, test } from "bun:test";
import { reviewFileIndex } from "./store-diff";
import type { ReviewFile } from "./lib/diff";

const files: ReviewFile[] = [
  { dir: "src/", name: "app.ts", add: 3, del: 1, lines: [] },
  { dir: "", name: "README.md", add: 1, del: 0, lines: [] },
];

test("reviewFileIndex matches repo-relative and absolute paths (both separators)", () => {
  expect(reviewFileIndex(files, "src/app.ts")).toBe(0);
  expect(reviewFileIndex(files, "C:\\work\\proj\\src\\app.ts")).toBe(0);
  expect(reviewFileIndex(files, "/home/u/proj/README.md")).toBe(1);
  expect(reviewFileIndex(files, "missing.ts")).toBe(-1);
});
