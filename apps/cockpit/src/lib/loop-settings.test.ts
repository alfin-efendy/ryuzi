import { describe, expect, test } from "bun:test";
import { normalizeLoopSetting } from "./loop-settings";

describe("normalizeLoopSetting", () => {
  test("accepts integers at or above the floor", () => {
    expect(normalizeLoopSetting("50", 1)).toBe("50");
    expect(normalizeLoopSetting(" 200 ", 1)).toBe("200");
    expect(normalizeLoopSetting("0", 0)).toBe("0"); // budget may be 0 (disabled)
  });
  test("rejects below-floor, non-numeric, and fractional input", () => {
    expect(normalizeLoopSetting("0", 1)).toBeNull();
    expect(normalizeLoopSetting("-3", 0)).toBeNull();
    expect(normalizeLoopSetting("abc", 1)).toBeNull();
    expect(normalizeLoopSetting("2.5", 1)).toBeNull();
    expect(normalizeLoopSetting("", 1)).toBeNull();
  });
});
