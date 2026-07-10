import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { CmdError, Result } from "@/bindings";

// Settings → Agent: AgentLoopCard persists two numeric settings via
// commands.getSetting/setSetting. A failed save must roll the input back to
// the last CONFIRMED-persisted value — not to whatever the user just typed
// (the bug this test guards: `onChange` mutates `values` on every keystroke,
// so reading `values[key]` at commit time is never the pre-edit value).
const MAX_TURNS_KEY = "agent.max_provider_turns";

let setSettingImpl: (key: string, value: string) => Promise<Result<null, CmdError>> = () =>
  Promise.resolve({ status: "error", error: { message: "boom" } });

const getSetting = mock((key: string): Promise<Result<string | null, CmdError>> => {
  if (key === MAX_TURNS_KEY) return Promise.resolve({ status: "ok", data: "50" });
  return Promise.resolve({ status: "ok", data: null });
});
const setSetting = mock((key: string, value: string): Promise<Result<null, CmdError>> => setSettingImpl(key, value));
const listToolPolicies = mock(() => Promise.resolve({ status: "ok", data: [] }));
const pickDirectory = mock((): Promise<string | null> => Promise.resolve(null));

mock.module("@/bindings", () => ({
  commands: { getSetting, setSetting, listToolPolicies, pickDirectory },
  // PermissionsCard pulls in `@/store`, which subscribes to core events on
  // module load (mirrors HomeView.test.tsx's bindings mock).
  events: { coreEventMsg: { listen: async () => () => {} } },
}));
// SettingsView reads the app version and the autostart-at-login state on
// mount; neither is under test here, so stub them out (mirrors HomeView.test.tsx
// stubbing @tauri-apps/api/webview for a hook it doesn't otherwise exercise).
mock.module("@tauri-apps/api/app", () => ({
  getVersion: () => Promise.resolve("0.0.0"),
}));
mock.module("@tauri-apps/plugin-autostart", () => ({
  isEnabled: () => Promise.resolve(false),
  enable: () => Promise.resolve(),
  disable: () => Promise.resolve(),
}));

const { SettingsView } = await import("./SettingsView");

beforeEach(() => {
  getSetting.mockClear();
  setSetting.mockClear();
  setSettingImpl = () => Promise.resolve({ status: "error", error: { message: "boom" } });
});

afterEach(() => {
  cleanup();
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
