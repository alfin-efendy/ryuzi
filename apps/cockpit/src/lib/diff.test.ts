import { describe, expect, test } from "bun:test";
import { parseUnifiedDiff } from "./diff";

const SAMPLE = `diff --git a/src/app.ts b/src/app.ts
index 111..222 100644
--- a/src/app.ts
+++ b/src/app.ts
@@ -1,4 +1,5 @@
 import x from "x";
-const a = 1;
+const a = 2;
+const b = 3;
 export default a;
diff --git a/README.md b/README.md
--- a/README.md
+++ b/README.md
@@ -10,2 +10,2 @@ ## Title
-old line
+new line
`;

describe("parseUnifiedDiff", () => {
  test("splits files and counts adds/dels", () => {
    const files = parseUnifiedDiff(SAMPLE);
    expect(files.length).toBe(2);
    expect(files[0].dir).toBe("src/");
    expect(files[0].name).toBe("app.ts");
    expect(files[0].add).toBe(2);
    expect(files[0].del).toBe(1);
    expect(files[1].name).toBe("README.md");
    expect(files[1].dir).toBe("");
  });

  test("tracks line numbers through hunks", () => {
    const files = parseUnifiedDiff(SAMPLE);
    const lines = files[0].lines;
    expect(lines[0][0]).toBe("hunk");
    // ctx "import x" is new line 1
    expect(lines[1]).toEqual(["ctx", 1, 'import x from "x";']);
    // del "const a = 1;" carries the OLD line number (2)
    expect(lines[2]).toEqual(["del", 2, "const a = 1;"]);
    // adds get consecutive new line numbers 2,3
    expect(lines[3]).toEqual(["add", 2, "const a = 2;"]);
    expect(lines[4]).toEqual(["add", 3, "const b = 3;"]);
    // trailing ctx continues the new counter (4)
    expect(lines[5]).toEqual(["ctx", 4, "export default a;"]);
  });

  test("empty diff yields no files", () => {
    expect(parseUnifiedDiff("")).toEqual([]);
  });
});
