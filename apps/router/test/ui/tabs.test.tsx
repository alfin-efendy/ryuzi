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

test("ConfigTab edits a setting via Enter + typing + Enter", async () => {
  const c = ctl();
  const { stdin, lastFrame } = render(<ConfigTab controller={c} setEditing={() => {}} />);
  await flush();
  expect(lastFrame()).toContain("discord_token");
  // move to default_effort, edit it
  // first key is the first row (discord_token); press Enter to edit it, type, submit
  stdin.write("\r"); await flush();         // enter edit on row 0
  stdin.write("zzz"); await flush();
  stdin.write("\r"); await flush();          // submit
  expect(c.get("discord_token")).toBe("zzz");
});
