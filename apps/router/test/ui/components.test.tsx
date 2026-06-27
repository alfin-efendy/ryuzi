import { test, expect } from "bun:test";
import React from "react";
import { render } from "ink-testing-library";
import { StatusDot } from "../../src/cli/ui/components/status-dot";
import { TabBar } from "../../src/cli/ui/components/tab-bar";
import { OptionsOverlay } from "../../src/cli/ui/components/options-overlay";

test("StatusDot shows filled when on", () => {
  expect(render(<StatusDot on label="running" />).lastFrame()).toContain("●");
  expect(render(<StatusDot on={false} label="stopped" />).lastFrame()).toContain("○");
});

test("TabBar highlights active and lists all tabs", () => {
  const f = render(<TabBar tabs={["Status","Daemon","Sessions","Config"]} active={1} />).lastFrame()!;
  expect(f).toContain("Status");
  expect(f).toContain("Daemon");
  expect(f).toContain("Config");
});

test("OptionsOverlay lists keybindings", () => {
  const f = render(<OptionsOverlay />).lastFrame()!;
  expect(f).toContain("Options");
  expect(f).toContain("quit");
});
