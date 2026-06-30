import { test, expect } from "bun:test";
import { composerMode } from "./composerMode";

test("running → stop; everything else → send", () => {
  expect(composerMode("running")).toBe("stop");
  expect(composerMode("idle")).toBe("send");
  expect(composerMode("interrupted")).toBe("send");
  expect(composerMode("ended")).toBe("send");
  expect(composerMode(undefined)).toBe("send");
});
