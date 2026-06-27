import { test, expect } from "bun:test";
import { execFileSync } from "node:child_process";

const launcher = `${import.meta.dir}/../../../../npm/harness-router/bin/hr.js`;

test("launcher execs the binary pointed to by HR_BINARY_PATH and forwards args + exit code", async () => {
  // Build a tiny native binary to stand in for the real hr.
  // bun build - --compile (stdin) is not supported in bun 1.3.x; write to a temp file first.
  const tmp = `${import.meta.dir}/fixture-echo`;
  const src = `console.log("ARGS=" + process.argv.slice(2).join(",")); process.exit(7);`;
  const tmpSrc = `${tmp}-src.ts`;
  await Bun.write(tmpSrc, src);
  const build = Bun.spawnSync(["bun", "build", tmpSrc, "--compile", "--outfile", tmp]);
  if (build.exitCode !== 0) throw new Error("fixture build failed: " + (build.stderr?.toString() ?? ""));

  let out = "";
  let code = 0;
  try {
    out = execFileSync("node", [launcher, "a", "b"], {
      env: { ...process.env, HR_BINARY_PATH: tmp },
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
