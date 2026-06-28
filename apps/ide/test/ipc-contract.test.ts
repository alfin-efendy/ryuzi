import { test, expect } from "bun:test";
import { IPC_COMMANDS } from "../src/shared/ipc-contract";

test("IPC_COMMANDS covers the 2a command surface", () => {
  expect([...IPC_COMMANDS]).toEqual([
    "listProjects",
    "getProject",
    "listSessions",
    "startSession",
    "continueSession",
    "stopSession",
    "endSession",
    "getConnId",
    "connectProject",
    "resolveApproval",
    "listConnections",
    "addConnection",
    "removeConnection",
    "selectConnection",
    "signIn",
    "signOut",
    "listDir",
    "readFile",
  ]);
});
