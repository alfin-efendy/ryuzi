import { beforeEach, expect, mock, spyOn, test } from "bun:test";
import type { ConnectionInfo } from "./bindings";
import { commands } from "./bindings";
import { useStore } from "./store";
import { useConnections } from "./store-connections";

const connection = {
  id: "account-1",
  provider: "openai-oauth",
  providerName: "ChatGPT",
  color: "#111111",
  initial: "C",
  authType: "oauth",
  label: "Personal",
  priority: 0,
  enabled: true,
  quotaCapability: "codex",
  models: ["gpt-5"],
  needsRelogin: false,
} satisfies ConnectionInfo;

const refreshModelConfiguration = mock(async () => {});

beforeEach(() => {
  refreshModelConfiguration.mockClear();
  useStore.setState({ refreshModelConfiguration });
  useConnections.setState({ connections: [connection], catalog: [], loaded: true });
});

test("rename uses the dedicated command, returns true, updates accounts, and refreshes model configuration once", async () => {
  const renamed = { ...connection, label: "Work" };
  const command = spyOn(commands, "renameConnection").mockResolvedValue({ status: "ok", data: [renamed] });

  expect(await useConnections.getState().rename(connection.id, "Work")).toBe(true);
  expect(command).toHaveBeenCalledWith(connection.id, "Work");
  expect(useConnections.getState().connections).toEqual([renamed]);
  expect(refreshModelConfiguration).toHaveBeenCalledTimes(1);
  command.mockRestore();
});

test("setEnabled uses the dedicated command and reports failure without refreshing", async () => {
  const command = spyOn(commands, "setConnectionEnabled").mockResolvedValue({
    status: "error",
    error: { message: "nope" },
  });

  expect(await useConnections.getState().setEnabled(connection.id, false)).toBe(false);
  expect(command).toHaveBeenCalledWith(connection.id, false);
  expect(useConnections.getState().connections).toEqual([connection]);
  expect(refreshModelConfiguration).not.toHaveBeenCalled();
  command.mockRestore();
});

test("setEnabled success updates accounts and refreshes structured model configuration exactly once", async () => {
  const disabled = { ...connection, enabled: false };
  const command = spyOn(commands, "setConnectionEnabled").mockResolvedValue({ status: "ok", data: [disabled] });

  expect(await useConnections.getState().setEnabled(connection.id, false)).toBe(true);
  expect(useConnections.getState().connections).toEqual([disabled]);
  expect(refreshModelConfiguration).toHaveBeenCalledTimes(1);
  command.mockRestore();
});

test("remove returns true only after the dedicated command succeeds and refreshes once", async () => {
  const command = spyOn(commands, "removeConnection").mockResolvedValue({ status: "ok", data: [] });

  expect(await useConnections.getState().remove(connection.id)).toBe(true);
  expect(command).toHaveBeenCalledWith(connection.id);
  expect(useConnections.getState().connections).toEqual([]);
  expect(refreshModelConfiguration).toHaveBeenCalledTimes(1);
  command.mockRestore();
});

test("rename failure returns false, preserves accounts, and does not refresh", async () => {
  const command = spyOn(commands, "renameConnection").mockResolvedValue({
    status: "error",
    error: { message: "duplicate" },
  });

  expect(await useConnections.getState().rename(connection.id, "Taken")).toBe(false);
  expect(useConnections.getState().connections).toEqual([connection]);
  expect(refreshModelConfiguration).not.toHaveBeenCalled();
  command.mockRestore();
});

test("dedicated account command rejection resolves false and does not refresh", async () => {
  const command = spyOn(commands, "removeConnection").mockRejectedValue(new Error("IPC unavailable"));

  expect(await useConnections.getState().remove(connection.id)).toBe(false);
  expect(useConnections.getState().connections).toEqual([connection]);
  expect(refreshModelConfiguration).not.toHaveBeenCalled();
  command.mockRestore();
});
