import { test, expect } from "bun:test";
import React from "react";
import { render } from "ink-testing-library";
import { AppController } from "../../src/cli/ui/controller";
import { Wizard } from "../../src/cli/ui/wizard";
import { detectClaude, detectGit } from "../../src/harness/detect";

const flush = () => new Promise((r) => setTimeout(r, 30));

test("wizard: pick gateway -> pick runtime -> fill fields -> done", async () => {
  // fresh db; migration seeds discord+claude-code enabled, but the wizard re-confirms selections
  const c = new AppController({ dbPath: ":memory:", detect: { claude: detectClaude, git: detectGit } });
  c.setEnabledGateways([]); c.setEnabledRuntimes([]); // start from blank selections
  let done = false;
  const { lastFrame, stdin } = render(<Wizard controller={c} onDone={() => { done = true; }} />);
  await flush();
  expect(lastFrame()).toContain("Choose gateways");
  stdin.write(" "); await flush();   // toggle the highlighted gateway (discord)
  stdin.write("\r"); await flush();  // confirm gateways -> runtimes step
  expect(lastFrame()).toContain("Choose runtimes");
  stdin.write(" "); await flush();   // toggle claude-code
  stdin.write("\r"); await flush();  // confirm runtimes -> fields step
  // required fields: discord.token, discord.app_id, discord.guild_id, workdir_root (order: provider then global, see impl)
  for (const v of ["tok", "app", "guild", "/repos"]) { stdin.write(v); await flush(); stdin.write("\r"); await flush(); }
  expect(done).toBe(true);
  expect(c.enabledGateways()).toEqual(["discord"]);
  expect(c.get("discord.token")).toBe("tok");
  expect(c.get("workdir_root")).toBe("/repos");
});
