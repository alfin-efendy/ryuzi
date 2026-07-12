import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type { CmdError, Result } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

const ok = <T,>(data: T): Result<T, CmdError> => ({ status: "ok", data });

// Settings → Agent renders three cards from one heading:
//   • AgentSection  — default-model picker, gated on store-agent's `loaded`.
//   • AgentLoopCard — two numeric settings via commands.getSetting/setSetting.
//   • PermissionsCard — pulls in @/store (core-event subscription on load).
// A single @/bindings mock therefore has to satisfy all three.
const MAX_TURNS_KEY = "agent.max_provider_turns";

type AgentSettings = { model: string | null; permMode: string | null };
let getAgentSettingsImpl: () => Promise<Result<AgentSettings, CmdError>> = () =>
  Promise.resolve(ok({ model: "anthropic/claude-opus-4", permMode: "ask" }));

const getAgentSettings = mock(() => getAgentSettingsImpl());
const setAgentSettings = mock(async (_runnerId: string, _model: string | null, _permMode: string | null) => ok(null));
const selectable = (requestValue: string) => ({
  kind: "concrete" as const,
  requestValue,
  displayName: requestValue,
  preferenceKey: null,
  supported: [],
  configuredDefault: null,
  resolvedDefault: null,
  defaultSource: "none" as const,
});
const listSelectableModels = mock(async (_runnerId: string) => ok([selectable("smart"), selectable("anthropic/claude-opus-4")]));

// AgentLoopCard: a failed save must roll the input back to the last CONFIRMED
// value — not to whatever the user just typed (the bug this guards: `onChange`
// mutates `values` on every keystroke, so reading `values[key]` at commit time
// is never the pre-edit value).
let setSettingImpl: (key: string, value: string) => Promise<Result<null, CmdError>> = () =>
  Promise.resolve({ status: "error", error: { message: "boom" } });

const WORKTREE_DIR_KEY = "worktree_dir";

const getSetting = mock((_runnerId: string, key: string): Promise<Result<string | null, CmdError>> => {
  if (key === MAX_TURNS_KEY) return Promise.resolve(ok("50"));
  if (key === WORKTREE_DIR_KEY) return Promise.resolve(ok("D:\\wt"));
  return Promise.resolve(ok(null));
});
const setSetting = mock((_runnerId: string, key: string, value: string): Promise<Result<null, CmdError>> => setSettingImpl(key, value));
const listToolPolicies = mock(() => Promise.resolve(ok([])));
const deleteToolPolicy = mock(() => Promise.resolve(ok(null)));
const pickDirectory = mock((): Promise<string | null> => Promise.resolve(null));
// SettingsView now mounts <AuditCard/>, which calls commands.listAudit on mount
// — stub it (empty feed) so mounting the view doesn't throw.
const listAudit = mock(async (_limit: number) => ok([]));

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
    listAudit,
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
  // Default: settings resolve immediately with a persisted default model, so
  // existing tests that don't care about the loading window keep working.
  getAgentSettingsImpl = () => Promise.resolve(ok({ model: "anthropic/claude-opus-4", permMode: "ask" }));
});

afterEach(() => {
  cleanup();
  useAgent.setState({ models: [], model: null, permMode: null, loaded: false });
  getAgentSettings.mockClear();
  setAgentSettings.mockClear();
  listSelectableModels.mockClear();
});

test("Agent section renders the default model picker, enabled once agent settings load", async () => {
  render(<SettingsView />);
  expect(screen.getByText("Agent")).toBeTruthy();
  // load() populated the picker with the persisted default model.
  await waitFor(() => expect(screen.getByRole("combobox", { name: "Default model" }).textContent).toContain("claude-opus-4"));
  expect(useAgent.getState().loaded).toBe(true);
  expect((screen.getByRole("combobox", { name: "Default model" }) as HTMLButtonElement).disabled).toBe(false);
});

test("the permission-mode row was dropped from the Agent card", async () => {
  render(<SettingsView />);
  await waitFor(() => expect(getAgentSettings).toHaveBeenCalled());
  expect(screen.queryByText("Permission mode")).toBeNull();
  for (const label of ["Plan", "Ask", "Edit", "Full"]) {
    expect(screen.queryByRole("button", { name: label })).toBeNull();
  }
});

test("changing the default model persists it with the hydrated permMode via set_agent_settings", async () => {
  render(<SettingsView />);
  await waitFor(() => expect(useAgent.getState().model).toBe("anthropic/claude-opus-4"));
  await useAgent.getState().setModel("smart");
  expect(setAgentSettings).toHaveBeenCalledWith(LOCAL_RUNNER, "smart", "ask");
});

test("the default model picker stays disabled when the agent-settings load fails, even though the model list is available", async () => {
  // Promise.all in store-agent's load() settles both calls together; a
  // real-world timeout/error on getAgentSettings can still land alongside a
  // successful listSelectableModels response, so models.length > 0 while
  // loaded stays false. The picker must render inert in that state, not
  // silently accept a click that would round-trip null model/permMode.
  getAgentSettingsImpl = () => Promise.resolve({ status: "error", error: { message: "boom" } });
  render(<SettingsView />);

  const picker = (await screen.findByRole("combobox", { name: "Default model" })) as HTMLButtonElement;
  await waitFor(() => expect(listSelectableModels).toHaveBeenCalled());
  expect(useAgent.getState().loaded).toBe(false);
  expect(picker.disabled).toBe(true);
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

test("Worktree folder shows the configured path and Browse saves a new one", async () => {
  render(<SettingsView />);
  const label = await screen.findByText("Worktree folder");
  const card = label.closest("div")?.parentElement?.parentElement as HTMLElement;
  await waitFor(() => expect(within(card).getByText("D:\\wt")).toBeTruthy());

  pickDirectory.mockResolvedValueOnce("E:\\other-wt");
  setSettingImpl = () => Promise.resolve({ status: "ok", data: null });
  fireEvent.click(within(card).getByRole("button", { name: "Browse" }));

  await waitFor(() => expect(setSetting).toHaveBeenCalledWith(LOCAL_RUNNER, WORKTREE_DIR_KEY, "E:\\other-wt"));
});
