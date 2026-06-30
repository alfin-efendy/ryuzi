import { test, expect } from "bun:test";
import { compareVersions, isNewer, parseVersion } from "../../src/update/version";

test("parseVersion strips a leading v and splits prerelease", () => {
  expect(parseVersion("v1.2.3")).toEqual({ major: 1, minor: 2, patch: 3, prerelease: [] });
  expect(parseVersion("1.2.3-rc.1")).toEqual({ major: 1, minor: 2, patch: 3, prerelease: ["rc", "1"] });
  expect(parseVersion("not-a-version")).toBeNull();
});

test("compareVersions orders by major/minor/patch", () => {
  expect(compareVersions("1.2.3", "1.2.3")).toBe(0);
  expect(compareVersions("1.2.3", "1.3.0")).toBe(-1);
  expect(compareVersions("2.0.0", "1.9.9")).toBe(1);
});

test("compareVersions ranks a release above its prerelease", () => {
  expect(compareVersions("1.2.0-rc.1", "1.2.0")).toBe(-1);
  expect(compareVersions("1.2.0", "1.2.0-rc.1")).toBe(1);
  expect(compareVersions("1.2.0-rc.1", "1.2.0-rc.2")).toBe(-1);
});

test("isNewer is true only when latest strictly exceeds current", () => {
  expect(isNewer("1.2.0", "1.3.0")).toBe(true);
  expect(isNewer("1.2.0", "v1.3.0")).toBe(true); // tolerates v-prefix
  expect(isNewer("1.2.0", "1.2.0")).toBe(false);
  expect(isNewer("1.3.0", "1.2.0")).toBe(false);
  expect(isNewer("1.2.0", "garbage")).toBe(false); // unparseable → no update
});

test("rejects trailing garbage but tolerates +build metadata", () => {
  expect(parseVersion("1.2.3.4")).toBeNull();
  expect(parseVersion("1.2.3-rc.1@bad")).toBeNull();
  expect(parseVersion("1.2.3+build.5")).toEqual({ major: 1, minor: 2, patch: 3, prerelease: [] });
  // a four-component string must NOT be read as a newer version
  expect(compareVersions("1.2.3.4", "1.2.0")).toBe(0);
});
