import { test, expect } from "bun:test";
import { buildCommands } from "../src/gateways/discord/commands";

test("defines connect/end/stop/status commands", () => {
  const names = buildCommands().map((c) => c.name);
  expect(names).toEqual(expect.arrayContaining(["connect", "end", "stop", "status"]));
});

test("connect has name/git/model/effort/mode options, mode constrained to perm modes", () => {
  const connect = buildCommands().find((c) => c.name === "connect")!;
  const opts = (connect.options ?? []) as Array<{ name: string; choices?: Array<{ value: string }> }>;
  expect(opts.map((o) => o.name)).toEqual(expect.arrayContaining(["name", "git", "model", "effort", "mode"]));
  const mode = opts.find((o) => o.name === "mode")!;
  expect((mode.choices ?? []).map((c) => c.value)).toEqual(["default", "acceptEdits", "bypassPermissions"]);
});
