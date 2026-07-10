import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { CmdError, Result } from "@/bindings";

const ok = <T,>(data: T): Result<T, CmdError> => ({ status: "ok", data });

// Settings → Agent renders three cards from one heading:
//   • AgentSection  — perm-mode segmented + default-model picker (store-agent).
//   • AgentLoopCard — two numeric settings via commands.getSetting/setSetting.
//   • PermissionsCard — pulls in @/store (core-event subscription on load).
// A single @/bindings mock therefore has to satisfy all three.
const MAX_TURNS_KEY = "agent.max_provider_turns";

const getAgentSettings = mock(async () => ok({ model: "anthropic/claude-opus-4", permMode: "ask" as string | null }));
const setAgentSettings = mock(async (_model: string | null, _permMode: string | null) => ok(null));
const listSelectableModels = mock(async () => ok(["smart", "anthropic/claude-opus-4"]));

// AgentLoopCard: a failed save must roll the input back to the last CONFIRMED
// value — not to whatever the user just typed (the bug this guards: `onChange`
// mutates `values` on every keystroke, so reading `values[key]` at commit time
// is never the pre-edit value).
let setSettingImpl: (key: string, value: string) => Promise<Result<null, CmdError>> = () =>
  Promise.resolve({ status: "error", error: { message: "boom" } });

const getSetting = mock((key: string): Promise<Result<string | null, CmdError>> => {
  if (key === MAX_TURNS_KEY) return Promise.resolve(ok("50"));
  return Promise.resolve(ok(null));
});
const setSetting = mock((key: string, value: string): Promise<Result<null, CmdError>> => setSettingImpl(key, value));
const listToolPolicies = mock(() => Promise.resolve(ok([])));
const deleteToolPolicy = mock(() => Promise.resolve(ok(null)));
const pickDirectory = mock((): Promise<string | null> => Promise.resolve(null));

// Mock the Tauri IPC boundary before the view (and the stores it pulls in) load.
mock.module("@/bindings", () => ({
  commands: {
    getSetting,
    setSetting,
    pickDirectory,
    listToolPolicies,
    deleteToolPolicy,
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

beforeEach(() => {
  getSetting.mockClear();
  setSetting.mockClear();
  setSettingImpl = () => Promise.resolve({ status: "error", error: { message: "boom" } });
});

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

test("failed save rolls back to the last persisted value, not the unsaved typed text", async () => {
  render(<SettingsView />);
  const input = (await screen.findByRole("textbox", { name: "Max provider turns" })) as HTMLInputElement;
  await waitFor(() => expect(input.value).toBe("50"));

  fireEvent.change(input, { target: { value: "1000" } });
  fireEvent.blur(input);

  // setSetting always errors in this test → must snap back to "50", the
  // value that was actually persisted — not "1000", the buggy no-op rollback.
  await waitFor(() => expect(input.value).toBe("50"));
});

test("a successful save is kept, and a later failed save rolls back to it", async () => {
  let calls = 0;
  setSettingImpl = () => {
    calls += 1;
    if (calls === 1) return Promise.resolve({ status: "ok", data: null });
    return Promise.resolve({ status: "error", error: { message: "boom" } });
  };

  render(<SettingsView />);
  const input = (await screen.findByRole("textbox", { name: "Max provider turns" })) as HTMLInputElement;
  await waitFor(() => expect(input.value).toBe("50"));

  fireEvent.change(input, { target: { value: "1000" } });
  fireEvent.blur(input);
  // First save succeeds — the normalized value is kept and becomes the new
  // confirmed baseline.
  await waitFor(() => expect(input.value).toBe("1000"));

  fireEvent.change(input, { target: { value: "2000" } });
  fireEvent.blur(input);
  // Second save fails — must roll back to "1000" (the last confirmed save),
  // not "50" (stale) and not "2000" (the unsaved typed text).
  await waitFor(() => expect(input.value).toBe("1000"));
});
