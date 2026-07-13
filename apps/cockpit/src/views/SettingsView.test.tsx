import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { CmdError, Result } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

const ok = <T,>(data: T): Result<T, CmdError> => ({ status: "ok", data });
const WORKTREE_DIR_KEY = "worktree_dir";

const getSetting = mock(
  (_runnerId: string, key: string): Promise<Result<string | null, CmdError>> =>
    Promise.resolve(ok(key === WORKTREE_DIR_KEY ? "D:\\wt" : null)),
);
const setSetting = mock(async (_runnerId: string, _key: string, _value: string) => ok(null));
const pickDirectory = mock((): Promise<string | null> => Promise.resolve(null));
const listAudit = mock(async (_limit: number) => ok([]));
const getAgent = mock(async () => ok(null));
const updateAgent = mock(async () => ok(null));
const listToolPolicies = mock(async () => ok([]));

mock.module("@/bindings", () => ({
  commands: {
    getSetting,
    setSetting,
    pickDirectory,
    listAudit,
    getAgent,
    updateAgent,
    listToolPolicies,
    deleteToolPolicy: mock(async () => ok(null)),
    listSelectableModels: mock(async () => ok([])),
  },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));
mock.module("@tauri-apps/api/app", () => ({ getVersion: async () => "0.5.0" }));
mock.module("@tauri-apps/plugin-autostart", () => ({
  isEnabled: async () => false,
  enable: async () => {},
  disable: async () => {},
}));

const { SettingsView } = await import("./SettingsView");

beforeEach(() => {
  for (const fn of [getSetting, setSetting, pickDirectory, listAudit, getAgent, updateAgent, listToolPolicies]) fn.mockClear();
});

afterEach(cleanup);

test("Settings contains no agent-owned controls", async () => {
  render(<SettingsView />);
  await waitFor(() => expect(listAudit).toHaveBeenCalledTimes(1));
  for (const text of ["Agent", "Default model", "Permissions", "Max provider turns", "Auto-continue budget"]) {
    expect(screen.queryByText(text)).toBeNull();
  }
  expect(screen.getByText("Appearance")).toBeTruthy();
  expect(screen.getAllByText("System").length).toBeGreaterThan(0);
});

test("Settings mount does not call agent registry or policy commands", async () => {
  render(<SettingsView />);
  await waitFor(() => expect(listAudit).toHaveBeenCalledTimes(1));
  expect(getAgent).not.toHaveBeenCalled();
  expect(updateAgent).not.toHaveBeenCalled();
  expect(listToolPolicies).not.toHaveBeenCalled();
});

test("About tagline tells the native story, not Claude Code", async () => {
  render(<SettingsView />);
  expect(screen.queryByText(/drive Claude Code/)).toBeNull();
  await waitFor(() => expect(screen.getByText(/drive the Ryuzi agent from chat and terminal/)).toBeTruthy());
});

test("Worktree folder shows the configured path and Browse saves a new one", async () => {
  render(<SettingsView />);
  const label = await screen.findByText("Worktree folder");
  const card = label.closest("div")?.parentElement?.parentElement as HTMLElement;
  await waitFor(() => expect(within(card).getByText("D:\\wt")).toBeTruthy());

  pickDirectory.mockResolvedValueOnce("E:\\other-wt");
  fireEvent.click(within(card).getByRole("button", { name: "Browse" }));

  await waitFor(() => expect(setSetting).toHaveBeenCalledWith(LOCAL_RUNNER, WORKTREE_DIR_KEY, "E:\\other-wt"));
});
