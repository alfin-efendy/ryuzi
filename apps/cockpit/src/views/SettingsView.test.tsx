import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { CmdError, Result } from "@/bindings";

const ok = <T,>(data: T): Result<T, CmdError> => ({ status: "ok", data });

const getAgentSettings = mock(async () => ok({ model: "anthropic/claude-opus-4", permMode: "ask" as string | null }));
const setAgentSettings = mock(async (_model: string | null, _permMode: string | null) => ok(null));
const listSelectableModels = mock(async () => ok(["smart", "anthropic/claude-opus-4"]));

// Mock the Tauri IPC boundary before the view (and the stores it pulls in) load.
mock.module("@/bindings", () => ({
  commands: {
    getSetting: async () => ok(null),
    setSetting: async () => ok(null),
    pickDirectory: async () => null,
    listToolPolicies: async () => ok([]),
    deleteToolPolicy: async () => ok(null),
    getAgentSettings,
    setAgentSettings,
    listSelectableModels,
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
const { useAgent } = await import("@/store-agent");

afterEach(() => {
  cleanup();
  useAgent.setState({ models: [], model: null, permMode: null });
  getAgentSettings.mockClear();
  setAgentSettings.mockClear();
  listSelectableModels.mockClear();
});

test("Agent section renders with perm-mode segmented + model picker from the store", async () => {
  render(<SettingsView />);
  expect(screen.getByText("Agent")).toBeTruthy();
  // Segmented renders one button per mode (idiom from RuntimeDetailView.test).
  for (const label of ["Plan", "Ask", "Edit", "Full"]) {
    expect(screen.getByRole("button", { name: label })).toBeTruthy();
  }
  // load() populated the picker with the persisted default model.
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Default model" }).textContent).toContain("claude-opus-4"));
});

test("picking a permission mode persists model + mode via set_agent_settings", async () => {
  render(<SettingsView />);
  await waitFor(() => expect(getAgentSettings).toHaveBeenCalled());
  await waitFor(() => expect(useAgent.getState().model).toBe("anthropic/claude-opus-4"));
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));
  await waitFor(() => expect(setAgentSettings).toHaveBeenCalledWith("anthropic/claude-opus-4", "edit"));
});

test("About tagline tells the native story, not Claude Code", async () => {
  render(<SettingsView />);
  expect(screen.queryByText(/drive Claude Code/)).toBeNull();
  await waitFor(() => expect(screen.getByText(/drive the Ryuzi agent from chat and terminal/)).toBeTruthy());
});
