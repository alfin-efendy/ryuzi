import { test, expect } from "bun:test";
import React from "react";
import { render } from "ink-testing-library";
import { Badge } from "../../src/cli/ui/components/badge";
import { StatusBar } from "../../src/cli/ui/components/status-bar";

test("Badge renders its text", () => {
  expect(render(<Badge tone="ok">running</Badge>).lastFrame()).toContain("running");
});

test("StatusBar renders all key hints", () => {
  const f = render(
    <StatusBar
      hints={[
        { k: "Tab", label: "switch" },
        { k: "q", label: "quit" },
      ]}
    />,
  ).lastFrame()!;
  expect(f).toContain("Tab");
  expect(f).toContain("switch");
  expect(f).toContain("quit");
});
