import { test, expect } from "bun:test";
import React from "react";
import { render } from "ink-testing-library";
import { AppController } from "../../src/cli/ui/controller";
import { Wizard } from "../../src/cli/ui/wizard";
import { detectClaude, detectGit } from "../../src/harness/detect";

const flush = () => new Promise((r) => setTimeout(r, 20));

test("wizard collects required settings step by step then calls onDone", async () => {
  const c = new AppController({ dbPath: ":memory:", detect: { claude: detectClaude, git: detectGit } });
  let done = false;
  const { stdin, lastFrame } = render(<Wizard controller={c} onDone={() => { done = true; }} />);
  await flush();
  expect(lastFrame()).toContain("hr setup");
  // 4 required keys: type a value + Enter for each
  for (const v of ["tok", "app", "guild", "/repos"]) { stdin.write(v); await flush(); stdin.write("\r"); await flush(); }
  expect(done).toBe(true);
  expect(c.get("discord_token")).toBe("tok");
  expect(c.get("workdir_root")).toBe("/repos");
});
