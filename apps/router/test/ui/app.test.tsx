import { test, expect } from "bun:test";
import React from "react";
import { render } from "ink-testing-library";
import { AppController } from "../../src/cli/ui/controller";
import { App } from "../../src/cli/ui/app";

const flush = () => new Promise((r) => setTimeout(r, 30));
function ctl() {
  return new AppController({
    dbPath: ":memory:",
    detect: { claude: async () => ({ found: true, version: "2" }), git: async () => ({ found: true, version: "2" }) },
  });
}
function configured() {
  const c = ctl();
  for (const [k, v] of [["discord_token","t"],["discord_app_id","a"],["discord_guild_id","g"],["workdir_root","/r"]] as const) c.set(k, v);
  return c;
}

test("unconfigured app starts in the wizard", async () => {
  const f = render(<App controller={ctl()} />);
  await flush();
  expect(f.lastFrame()).toContain("hr setup");
});

test("configured app shows the dashboard and switches tabs", async () => {
  const { stdin, lastFrame } = render(<App controller={configured()} />);
  await flush();
  expect(lastFrame()).toContain("Status");
  stdin.write("2"); await flush();           // jump to Daemon tab
  expect(lastFrame()).toContain("press s to start");
});

test("? toggles the options overlay", async () => {
  const { stdin, lastFrame } = render(<App controller={configured()} />);
  await flush();
  stdin.write("?"); await flush();
  expect(lastFrame()).toContain("Options");
});
