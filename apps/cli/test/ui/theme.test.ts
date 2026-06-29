// apps/router/test/ui/theme.test.ts
import { test, expect } from "bun:test";
import { palette, symbols, borderStyle, colorEnabled, paint } from "../../src/cli/ui/theme";

test("palette exposes hex tokens", () => {
  expect(palette.signature).toBe("#ff2d95");
  expect(palette.accent).toBe("#37c9e6");
  expect(palette.ok).toBe("#36f9c3");
});

test("symbols default to unicode, ascii only under HR_ASCII", () => {
  delete process.env.HR_ASCII;
  expect(symbols().dot).toBe("●");
  expect(borderStyle()).toBe("round");
  process.env.HR_ASCII = "1";
  expect(symbols().dot).toBe("*");
  expect(borderStyle()).toBe("single");
  delete process.env.HR_ASCII;
});

test("paint is plain when color disabled (non-TTY test env)", () => {
  // bun test stdout is not a TTY → colorEnabled() is false → no escape codes
  expect(colorEnabled()).toBe(false);
  expect(paint("PASS", "ok", { bold: true })).toBe("PASS");
});
