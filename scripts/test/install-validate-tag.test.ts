import { test, expect } from "bun:test";
import { $ } from "bun";

// Running the installer with a malformed explicit version must fail fast,
// before any download, and must not execute injected commands.
test("install.sh rejects a malformed HR_VERSION before downloading", async () => {
  const marker = `pwned-${process.pid}`;
  const res = await $`sh ./install.sh`
    .env({ ...process.env, HR_VERSION: `v9.9.9; touch ${marker}`, HR_INSTALL_DIR: "/tmp/hr-test-bin" })
    .nothrow()
    .quiet();
  expect(res.exitCode).not.toBe(0);
  expect(res.stderr.toString()).toContain("invalid version");
  // the injected command must not have run
  expect(await Bun.file(marker).exists()).toBe(false);
});

test("install.sh rejects a path-traversal HR_VERSION (offline)", async () => {
  // Slashes are disallowed, so traversal never reaches a URL. Rejected before
  // any network call.
  const res = await $`sh ./install.sh`
    .env({ ...process.env, HR_VERSION: "../../../tmp/evil", HR_INSTALL_DIR: "/tmp/hr-test-bin" })
    .nothrow()
    .quiet();
  expect(res.exitCode).not.toBe(0);
  expect(res.stderr.toString()).toContain("invalid version");
});

test("install.sh does NOT over-reject a well-formed version format", async () => {
  // A valid-format tag (incl. an -rc suffix) must pass validation. It then
  // fails later at the network/download step (404), which is fine — we only
  // assert it got PAST validation (guards against an over-strict regex).
  const res = await $`sh ./install.sh`
    .env({ ...process.env, HR_VERSION: "v0.0.0-rc.1", HR_INSTALL_DIR: "/tmp/hr-test-bin" })
    .nothrow()
    .quiet();
  expect(res.stderr.toString()).not.toContain("invalid version");
});
