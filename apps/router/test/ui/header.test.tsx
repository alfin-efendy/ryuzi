import { test, expect } from "bun:test";
import React from "react";
import { render } from "ink-testing-library";
import { Header } from "../../src/cli/ui/components/header";

test("Header shows brand + tabs and omits the 'hr' label", () => {
  const f = render(<Header tabs={["Status", "Daemon", "Sessions", "Config"]} active={0} />).lastFrame()!;
  expect(f).toContain("マ Harness Router");
  expect(f).toContain("Status");
  expect(f).toContain("Config");
  expect(f).not.toMatch(/Harness Router\s+hr\b/); // no "hr" tag next to brand
});
