import { test, expect } from "bun:test";
import { collectOpenDirs, type Node } from "./FileTreePane";

const dir = (rel: string, open: boolean, children?: Node[]): Node => ({ rel, name: rel, dir: true, depth: 0, open, children });
const file = (rel: string): Node => ({ rel, name: rel, dir: false, depth: 1 });

test("collectOpenDirs walks only expanded directories", () => {
  const tree = [dir("src", true, [dir("src/lib", true, [file("src/lib/a.ts")]), dir("src/views", false)]), dir("docs", false)];
  expect(collectOpenDirs(tree)).toEqual(["src", "src/lib"]);
});
