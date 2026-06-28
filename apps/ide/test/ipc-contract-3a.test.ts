import { test, expect } from "bun:test";
import { IPC_COMMANDS } from "../src/shared/ipc-contract";

test("IPC_COMMANDS includes the 3a file commands", () => {
  expect(IPC_COMMANDS).toContain("listDir");
  expect(IPC_COMMANDS).toContain("readFile");
});
