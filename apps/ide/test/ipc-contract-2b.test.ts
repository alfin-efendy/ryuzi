import { test, expect } from "bun:test";
import { IPC_COMMANDS, APPROVAL_CHANNEL } from "../src/shared/ipc-contract";

test("IPC_COMMANDS includes the 2b commands", () => {
  expect(IPC_COMMANDS).toContain("connectProject");
  expect(IPC_COMMANDS).toContain("resolveApproval");
});

test("APPROVAL_CHANNEL constant is defined", () => {
  expect(APPROVAL_CHANNEL).toBe("harness:approval");
});
