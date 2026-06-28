import { test, expect } from "bun:test";
import React from "react";
import { render } from "ink-testing-library";
import { Text } from "ink";
import { Panel } from "../../src/cli/ui/components/panel";
import { StatusDot } from "../../src/cli/ui/components/status-dot";

test("Panel shows its title and children", () => {
  const f = render(
    <Panel title="Services">
      <Text>Daemon</Text>
    </Panel>,
  ).lastFrame()!;
  expect(f).toContain("SERVICES"); // titles render uppercase
  expect(f).toContain("Daemon");
});

test("StatusDot still shows filled/empty markers", () => {
  expect(render(<StatusDot on label="running" />).lastFrame()).toContain("●");
  expect(render(<StatusDot on={false} />).lastFrame()).toContain("○");
});
