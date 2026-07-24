import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ComponentOauthProfileInfo } from "@/bindings";

const beginDeviceFlow = mock(
  async (_r: string | null, _p: string, _pr: string, _u: string) => ({
    status: "ok" as const,
    data: {
      deviceCode: "dev-code-xyz",
      userCode: "WXYZ-1234",
      verificationUri: "https://github.com/login/device",
      verificationUriComplete: null as string | null,
      intervalSecs: 0,
      expiresAt: Date.now() + 600_000,
    },
  }),
);
// Each entry is either an ok-outcome string or the literal "ERR" to simulate a
// transient RPC failure (the store maps that to a `null` outcome).
let pollOutcomes: string[] = ["ready"];
const pollDeviceFlow = mock(async () => {
  const next = pollOutcomes.shift() ?? "ready";
  return next === "ERR"
    ? { status: "error" as const, error: { message: "error sending request" } }
    : { status: "ok" as const, data: next };
});
const disconnect = mock(async () => ({ status: "ok" as const, data: null }));
const openUrl = mock(async (_u: string) => {});
const toastSuccess = mock((_m: string) => {});

mock.module("@/bindings", () => ({
  commands: {
    pluginProfileBeginDeviceFlow: beginDeviceFlow,
    pluginProfilePollDeviceFlow: pollDeviceFlow,
    pluginProfileDisconnect: disconnect,
  },
}));
mock.module("sonner", () => ({
  toast: { success: toastSuccess, error: mock(() => {}), warning: mock(() => {}), info: mock(() => {}) },
  Toaster: () => null,
}));
mock.module("@tauri-apps/plugin-opener", () => ({ openUrl }));

// Imported AFTER the mocks so the store's `commands` binding is the mock.
const { OauthProfileConnections, isDeviceFlowConnectable } = await import("./OauthProfileConnections");

function profile(over: Partial<ComponentOauthProfileInfo> = {}): ComponentOauthProfileInfo {
  return {
    id: "github",
    scopes: ["repo"],
    tokenUrl: "https://github.com/login/oauth/access_token",
    deviceAuthorizationUrl: "https://github.com/login/device/code",
    connected: false,
    clientIdConfigured: true,
    ...over,
  };
}

beforeEach(() => {
  pollOutcomes = ["ready"];
  beginDeviceFlow.mockClear();
  pollDeviceFlow.mockClear();
  disconnect.mockClear();
  openUrl.mockClear();
  toastSuccess.mockClear();
});
afterEach(cleanup);

test("isDeviceFlowConnectable requires both device-authorization and token URLs", () => {
  expect(isDeviceFlowConnectable(profile())).toBe(true);
  expect(isDeviceFlowConnectable(profile({ deviceAuthorizationUrl: null }))).toBe(false);
  expect(isDeviceFlowConnectable(profile({ tokenUrl: null }))).toBe(false);
});

test("renders nothing when no profile is device-flow connectable", () => {
  const { container } = render(
    <OauthProfileConnections pluginId="github" profiles={[profile({ deviceAuthorizationUrl: null })]} onChanged={() => {}} />,
  );
  expect(container.firstChild).toBeNull();
});

test("a not-connected connectable profile shows an enabled Connect button", () => {
  render(<OauthProfileConnections pluginId="github" profiles={[profile()]} onChanged={() => {}} />);
  expect(screen.getByText("Not connected")).toBeTruthy();
  const btn = screen.getByRole("button", { name: "Connect" }) as HTMLButtonElement;
  expect(btn.disabled).toBe(false);
});

test("Connect is disabled with a hint when no client id is configured", () => {
  render(<OauthProfileConnections pluginId="github" profiles={[profile({ clientIdConfigured: false })]} onChanged={() => {}} />);
  expect((screen.getByRole("button", { name: "Connect" }) as HTMLButtonElement).disabled).toBe(true);
  expect(screen.getByText(/No OAuth client id is configured/)).toBeTruthy();
});

test("a connected profile shows Disconnect and calls disconnect + onChanged", async () => {
  const onChanged = mock(() => {});
  render(<OauthProfileConnections pluginId="github" profiles={[profile({ connected: true })]} onChanged={onChanged} />);
  expect(screen.getByText("Connected")).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Disconnect" }));
  await waitFor(() => expect(disconnect).toHaveBeenCalledWith("local", "github", "github"));
  await waitFor(() => expect(onChanged).toHaveBeenCalled());
});

test("Connect runs the device flow to Ready: shows the code, polls, then reports success", async () => {
  const onChanged = mock(() => {});
  render(<OauthProfileConnections pluginId="github" profiles={[profile()]} onChanged={onChanged} />);

  fireEvent.click(screen.getByRole("button", { name: "Connect" }));

  // The user code is shown immediately after begin resolves.
  expect(await screen.findByText("WXYZ-1234")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: /Open sign-in/ }));
  expect(openUrl).toHaveBeenCalledWith("https://github.com/login/device");

  // Poll returns "ready" → success + refresh.
  await waitFor(() => expect(pollDeviceFlow).toHaveBeenCalled(), { timeout: 3000 });
  await waitFor(() => expect(toastSuccess).toHaveBeenCalledWith("Connected github"), { timeout: 3000 });
  await waitFor(() => expect(onChanged).toHaveBeenCalled());
});

test("a denied outcome surfaces an error state with a retry", async () => {
  pollOutcomes = ["denied"];
  render(<OauthProfileConnections pluginId="github" profiles={[profile()]} onChanged={() => {}} />);
  fireEvent.click(screen.getByRole("button", { name: "Connect" }));

  await waitFor(() => expect(screen.getByText(/authorization was declined/)).toBeTruthy(), { timeout: 3000 });
  expect(screen.getByRole("button", { name: "Try again" })).toBeTruthy();
});

test("a transient poll error is tolerated: the flow keeps polling and still connects", async () => {
  // First poll is a network blip (ERR → null), the next reports ready. The flow
  // must NOT abort on the blip.
  pollOutcomes = ["ERR", "ready"];
  const onChanged = mock(() => {});
  render(<OauthProfileConnections pluginId="github" profiles={[profile()]} onChanged={onChanged} />);
  fireEvent.click(screen.getByRole("button", { name: "Connect" }));

  await waitFor(() => expect(toastSuccess).toHaveBeenCalledWith("Connected github"), { timeout: 4000 });
  await waitFor(() => expect(onChanged).toHaveBeenCalled());
  expect(pollDeviceFlow.mock.calls.length).toBeGreaterThanOrEqual(2);
});
