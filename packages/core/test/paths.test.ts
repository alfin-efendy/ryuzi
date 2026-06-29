import { test, expect } from "bun:test";
import { expandHome } from "../src/config/paths";

test("expandHome expands leading ~ to HOME, leaves others", () => {
  const home = process.env.HOME ?? "";
  expect(expandHome("~/sandbox-test")).toBe(`${home}/sandbox-test`);
  expect(expandHome("~")).toBe(home);
  expect(expandHome("/abs/path")).toBe("/abs/path");
  expect(expandHome("relative/dir")).toBe("relative/dir");
  expect(expandHome("~user/x")).toBe("~user/x"); // only `~` and `~/...` expand, not `~user`
});
