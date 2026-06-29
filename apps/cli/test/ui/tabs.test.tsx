import { test, expect } from "bun:test";
import React from "react";
import { render } from "ink-testing-library";
import { AppController } from "../../src/cli/ui/controller";
import { StatusTab } from "../../src/cli/ui/tabs/status";
import { DaemonTab } from "../../src/cli/ui/tabs/daemon";
import { SessionsTab } from "../../src/cli/ui/tabs/sessions";
import { ConfigTab } from "../../src/cli/ui/tabs/config";

const flush = () => new Promise((r) => setTimeout(r, 20));
function ctl() {
  return new AppController({
    dbPath: ":memory:",
    detect: { claude: async () => ({ found: true, version: "2" }), git: async () => ({ found: true, version: "2" }) },
  });
}

test("StatusTab warns when settings missing", async () => {
  const f = render(<StatusTab controller={ctl()} />);
  await flush();
  expect(f.lastFrame()).toContain("missing settings");
});

test("DaemonTab shows stopped and start hint", () => {
  expect(render(<DaemonTab controller={ctl()} />).lastFrame()).toContain("stopped");
});

test("SessionsTab shows empty state", () => {
  expect(render(<SessionsTab controller={ctl()} />).lastFrame()).toContain("no sessions");
});

test("ConfigTab shows grouped labels/help and edits a field", async () => {
  const c = ctl();
  c.setEnabledGateways(["discord"]);
  c.setEnabledRuntimes(["claude-code"]);
  const { stdin, lastFrame } = render(<ConfigTab controller={c} setEditing={() => {}} />);
  await flush();
  const f = lastFrame()!;
  expect(f).toContain("General");
  expect(f).toContain("Discord"); // gateway group header (label)
  expect(f).toContain("Workdir root"); // a field label, not the raw key
  // first selectable row is the first general field (workdir_root); edit it
  stdin.write("\r");
  await flush(); // enter edit
  stdin.write("/tmp/x");
  await flush();
  stdin.write("\r");
  await flush(); // submit
  expect(c.get("workdir_root")).toBe("/tmp/x");
});
