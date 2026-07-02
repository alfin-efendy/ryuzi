import { describe, expect, test } from "bun:test";
import { accentVars, parseAccent, resolveBackdropAttr } from "./theme";

describe("resolveBackdropAttr", () => {
  test("returns capability when transparency is on", () => {
    expect(resolveBackdropAttr("mica", true)).toBe("mica");
    expect(resolveBackdropAttr("vibrancy", true)).toBe("vibrancy");
  });
  test("returns null when transparency is off or unsupported", () => {
    expect(resolveBackdropAttr("mica", false)).toBeNull();
    expect(resolveBackdropAttr("none", true)).toBeNull();
    expect(resolveBackdropAttr("none", false)).toBeNull();
  });
});

describe("parseAccent", () => {
  test("accepts system", () => {
    expect(parseAccent("system")).toBe("system");
  });
  test("accepts preset keys and custom hex, rejects garbage", () => {
    expect(parseAccent("blue")).toBe("blue");
    expect(parseAccent("#a1b2c3")).toEqual({ custom: "#a1b2c3" });
    expect(parseAccent("bogus")).toBe("neutral");
    expect(parseAccent(null)).toBe("neutral");
  });
});

describe("accentVars with system accent", () => {
  test("system + known hex behaves like custom", () => {
    expect(accentVars("system", "#0078d4")).toEqual(accentVars({ custom: "#0078d4" }));
  });
  test("system without hex falls back to neutral (no overrides)", () => {
    expect(accentVars("system", null)).toEqual({});
    expect(accentVars("system")).toEqual({});
  });
});
