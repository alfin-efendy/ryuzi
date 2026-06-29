import { test, expect } from "bun:test";
import { helpText } from "../../src/cli/meta";

test("help text is plain under non-TTY and keeps its sections", () => {
  const h = helpText();
  expect(h).toContain("USAGE");
  expect(h).toContain("COMMANDS");
  expect(h).toContain("doctor");
  expect(h).not.toContain("\x1b["); // no escapes in non-TTY test env
});
