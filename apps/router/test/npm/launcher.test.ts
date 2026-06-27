import { afterAll, test, expect } from "bun:test";
import { execFileSync } from "node:child_process";
import { mkdtempSync, rmSync } from "node:fs";
import { join } from "node:path";
import { tmpdir } from "node:os";

const launcher = `${import.meta.dir}/../../../../npm/harness-router/bin/hr.js`;

// Build fixture in a temp directory outside the repo so no artifacts remain after the run.
const fixtureDir = mkdtempSync(join(tmpdir(), "hr-launcher-"));
const fixtureBin = join(fixtureDir, "fixture-echo");
const fixtureSrc = join(fixtureDir, "fixture-echo-src.ts");

// bun build - --compile (stdin) is not supported in bun 1.3.x; write to a temp file first.
const src = `console.log("ARGS=" + process.argv.slice(2).join(",")); process.exit(7);`;
await Bun.write(fixtureSrc, src);
const build = Bun.spawnSync(["bun", "build", fixtureSrc, "--compile", "--outfile", fixtureBin]);
if (build.exitCode !== 0) throw new Error("fixture build failed: " + (build.stderr?.toString() ?? ""));

afterAll(() => {
  rmSync(fixtureDir, { recursive: true, force: true });
});

test("launcher execs the binary pointed to by HR_BINARY_PATH and forwards args + exit code", async () => {
  let out = "";
  let code = 0;
  try {
    out = execFileSync("node", [launcher, "a", "b"], {
      env: { ...process.env, HR_BINARY_PATH: fixtureBin },
      encoding: "utf8",
    });
  } catch (e: any) {
    out = (e.stdout ?? "").toString();
    code = e.status;
  }
  expect(out).toContain("ARGS=a,b");
  expect(code).toBe(7);
});

test("launcher errors clearly when no binary is found", () => {
  let stderr = "";
  let code = 0;
  try {
    execFileSync("node", [launcher], {
      env: { ...process.env, HR_BINARY_PATH: "/nonexistent/definitely-not-here" },
      encoding: "utf8",
    });
  } catch (e: any) {
    stderr = (e.stderr ?? "").toString();
    code = e.status;
  }
  // With a bogus override that doesn't exist, the launcher falls through to
  // package resolution, which fails (the platform package isn't installed in this repo).
  expect(code).toBe(1);
  expect(stderr).toMatch(/harness-router/i);
});
