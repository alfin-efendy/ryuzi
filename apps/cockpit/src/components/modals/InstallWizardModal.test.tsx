import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { CmdError, PluginDetail, PluginFieldInfo, PluginInstallBeginResult, Result } from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

// The wizard talks only to the Tauri IPC boundary (`@/bindings`) and the
// real usePlugins zustand store — mock the boundary, seed the store.

const tokenAuthInfo = {
  kind: "token",
  setting: "plugin.notion.token",
  env: "NOTION_TOKEN",
  helpUrl: "https://notion.example/help",
  configured: false,
  oauthConnectAvailable: false,
  oauthConnectError: null,
  oauthTokenStored: false,
  oauthReconnectRequired: false,
};

const oauthAuthInfo = {
  kind: "oauth",
  setting: null,
  env: null,
  helpUrl: "https://notion.example/oauth-help",
  configured: false,
  oauthConnectAvailable: true,
  oauthConnectError: null,
  oauthTokenStored: false,
  oauthReconnectRequired: false,
};

function field(key: string, label: string, overrides: Partial<PluginFieldInfo> = {}): PluginFieldInfo {
  return { key, label, help: "", secret: false, required: true, valueSet: false, ...overrides };
}

function detailFixture(
  overrides: { experimental?: boolean; auth?: PluginDetail["auth"]; settings?: PluginFieldInfo[] } = {},
): PluginDetail {
  return {
    info: {
      id: "notion",
      name: "Notion",
      description: "Notion MCP",
      icon: null,
      categories: ["docs"],
      verified: true,
      experimental: overrides.experimental ?? false,
      enabled: false,
      configured: false,
      source: "catalog",
      capabilities: ["connector"],
      kind: "integration",
      installed: false,
      family: null,
      pinned: false,
      sourceSpec: null,
      resolvedCommit: null,
      installedAt: null,
      updatedAt: null,
      trustTier: null,
    },
    auth: overrides.auth ?? null,
    settings: overrides.settings ?? [],
    mcp: [],
    models: [],
    homepage: null,
    publisher: "Notion",
  };
}

function beginResult(overrides: Partial<PluginInstallBeginResult> = {}): PluginInstallBeginResult {
  return {
    authKind: "none",
    envVarPresent: false,
    envVarName: null,
    oauthAvailable: false,
    oauthExternal: false,
    needsClientId: false,
    dcrSucceeded: false,
    callbackMode: "auto",
    oauthBegin: null,
    dcrError: null,
    ...overrides,
  };
}

const oauthBegin = {
  stateToken: "state-123",
  authorizeUrl: "https://vendor.example.com/oauth/authorize?client_id=abc",
  redirectUri: "http://127.0.0.1:8976/plugin-oauth/notion/callback",
};

// Mutable fixtures read by the mocks at call time — tests set these before
// rendering (and swap beginData mid-test to simulate a re-begin).
let detailData: PluginDetail = detailFixture();
let beginData: PluginInstallBeginResult = beginResult();

const ok = <T,>(data: T) => Promise.resolve({ status: "ok" as const, data });

const pluginDetail = mock((_id: string): Promise<Result<PluginDetail, CmdError>> => ok(detailData));
const beginPluginInstall = mock((_pluginId: string): Promise<Result<PluginInstallBeginResult, CmdError>> => ok(beginData));
const setPluginOauthClientId = mock((_pluginId: string, _clientId: string): Promise<Result<null, CmdError>> => ok(null));
const cancelPluginInstall = mock((_pluginId: string, _stateToken: string | null) => ok(null));
const completePluginOauth = mock((_pluginId: string, _code: string, _stateToken: string) => ok(oauthAuthInfo));
const setPluginSetting = mock((_key: string, _value: string) => ok(null));
const setPluginEnabled = mock((_id: string, _enabled: boolean) => ok(null));
const listPlugins = mock(() => ok([]));
const pluginsRestartRequired = mock(() => ok(false));
const openUrl = mock(async (_url: string) => {});
const toastError = mock((_message: string) => {});

type CompletedEvent = { payload: { pluginId: string; ok: boolean; error: string | null } };
let completedListener: ((event: CompletedEvent) => void) | null = null;
const pluginOauthCompletedMsgListen = mock(async (cb: (event: CompletedEvent) => void) => {
  completedListener = cb;
  return () => {
    completedListener = null;
  };
});

mock.module("@/bindings", () => ({
  events: {
    pluginOauthCompletedMsg: { listen: pluginOauthCompletedMsgListen },
  },
  commands: {
    pluginDetail,
    beginPluginInstall,
    setPluginOauthClientId,
    cancelPluginInstall,
    completePluginOauth,
    setPluginSetting,
    setPluginEnabled,
    listPlugins,
    pluginsRestartRequired,
  },
}));
mock.module("@tauri-apps/plugin-opener", () => ({ openUrl }));
// @ryuzi/ui's barrel also re-exports sonner's `Toaster` (unused here, but
// its module-eval import must resolve), so the mock stubs it too.
mock.module("sonner", () => ({
  toast: { error: toastError, success: mock(() => {}), info: mock(() => {}), warning: mock(() => {}) },
  Toaster: () => null,
}));

const { InstallWizardModal } = await import("./InstallWizardModal");
const { usePlugins } = await import("@/store-plugins");

const onClose = mock(() => {});

async function renderWizard() {
  const result = render(<InstallWizardModal pluginId="notion" pluginName="Notion" pluginIcon={null} onClose={onClose} />);
  await act(async () => {});
  return result;
}

beforeEach(() => {
  detailData = detailFixture();
  beginData = beginResult();
  completedListener = null;
  pluginDetail.mockClear();
  beginPluginInstall.mockClear();
  setPluginOauthClientId.mockClear();
  cancelPluginInstall.mockClear();
  completePluginOauth.mockClear();
  setPluginSetting.mockClear();
  setPluginEnabled.mockClear();
  listPlugins.mockClear();
  pluginsRestartRequired.mockClear();
  openUrl.mockClear();
  toastError.mockClear();
  pluginOauthCompletedMsgListen.mockClear();
  onClose.mockClear();
  usePlugins.setState({
    plugins: [],
    loaded: false,
    restartRequired: false,
    doctorFindings: [],
    doctorLoaded: false,
  });
});

afterEach(() => {
  cleanup();
  usePlugins.setState({
    plugins: [],
    loaded: false,
    restartRequired: false,
    doctorFindings: [],
    doctorLoaded: false,
  });
});

test("calls beginPluginInstall on mount and shows the checking spinner", async () => {
  beginPluginInstall.mockImplementationOnce(() => new Promise<never>(() => {}));
  await renderWizard();

  expect(beginPluginInstall).toHaveBeenCalledWith(LOCAL_RUNNER, "notion");
  expect(screen.getByText("Checking configuration…")).toBeTruthy();
});

test("checking shows the oauth spinner copy while sign-in is being prepared", async () => {
  detailData = detailFixture({ auth: oauthAuthInfo });
  beginPluginInstall.mockImplementationOnce(() => new Promise<never>(() => {}));
  await renderWizard();

  expect(screen.getByText("Preparing sign-in…")).toBeTruthy();
});

test("envVarPresent routes to settings when the manifest declares settings", async () => {
  detailData = detailFixture({
    auth: { ...tokenAuthInfo, kind: "api-key" },
    settings: [field("plugin.notion.app_key", "Application key", { secret: true })],
  });
  beginData = beginResult({ authKind: "api-key", envVarPresent: true, envVarName: "DD_API_KEY" });
  await renderWizard();

  expect(await screen.findByText(/Required fields are marked/)).toBeTruthy();
});

test("envVarPresent with no settings routes straight to done", async () => {
  beginData = beginResult({ authKind: "token", envVarPresent: true, envVarName: "NOTION_TOKEN" });
  await renderWizard();

  expect(await screen.findByText("Notion is installed.")).toBeTruthy();
});

test("token auth routes to tokenInput", async () => {
  detailData = detailFixture({ auth: tokenAuthInfo });
  beginData = beginResult({ authKind: "token" });
  await renderWizard();

  expect(await screen.findByText(/authenticates with a token/)).toBeTruthy();
});

test("api-key auth routes to tokenInput with API-key wording", async () => {
  detailData = detailFixture({ auth: { ...tokenAuthInfo, kind: "api-key" } });
  beginData = beginResult({ authKind: "api-key" });
  await renderWizard();

  expect(await screen.findByText(/authenticates with an API key/)).toBeTruthy();
});

test("authKind none routes through settings when declared", async () => {
  detailData = detailFixture({ settings: [field("plugin.notion.user", "User")] });
  beginData = beginResult({ authKind: "none" });
  await renderWizard();

  expect(await screen.findByText(/Required fields are marked/)).toBeTruthy();
});

test("available oauth routes to waitingOauth", async () => {
  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  await renderWizard();

  expect(await screen.findByText("Browser opened — finish signing in there.")).toBeTruthy();
});

test("needsClientId routes to manualClientId", async () => {
  beginData = beginResult({ authKind: "oauth", needsClientId: true });
  await renderWizard();

  expect(await screen.findByText(/paste its client ID here/)).toBeTruthy();
});

test("oauthExternal routes to manualClientId", async () => {
  beginData = beginResult({ authKind: "oauth", oauthExternal: true, needsClientId: true });
  await renderWizard();

  expect(await screen.findByText(/brokers its own sign-in/)).toBeTruthy();
});

test("oauth discovery failure shows the error with Retry, and Retry re-begins", async () => {
  beginData = beginResult({ authKind: "oauth", dcrError: "discovery failed: 404 on both forms" });
  await renderWizard();

  expect(await screen.findByText("discovery failed: 404 on both forms")).toBeTruthy();

  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));

  expect(await screen.findByText("Browser opened — finish signing in there.")).toBeTruthy();
  await waitFor(() => expect(beginPluginInstall).toHaveBeenCalledTimes(2));
});

test("Cancel closes the wizard", async () => {
  beginPluginInstall.mockImplementationOnce(() => new Promise<never>(() => {}));
  await renderWizard();

  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(onClose).toHaveBeenCalled();
});

test("tokenInput saves through the manifest auth.setting key and continues", async () => {
  detailData = detailFixture({ auth: tokenAuthInfo });
  beginData = beginResult({ authKind: "token" });
  await renderWizard();

  const cont = screen.getByRole("button", { name: "Continue" }) as HTMLButtonElement;
  expect(cont.disabled).toBe(true);

  fireEvent.change(screen.getByPlaceholderText("Required — not set"), { target: { value: "secret-token" } });
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));

  await waitFor(() => expect(setPluginSetting).toHaveBeenCalledWith(LOCAL_RUNNER, "plugin.notion.token", "secret-token"));
  expect(await screen.findByText("Notion is installed.")).toBeTruthy();
});

test("tokenInput routes to settings after saving when settings are declared", async () => {
  detailData = detailFixture({ auth: tokenAuthInfo, settings: [field("plugin.notion.user", "User")] });
  beginData = beginResult({ authKind: "token" });
  await renderWizard();

  fireEvent.change(screen.getByPlaceholderText("Required — not set"), { target: { value: "secret-token" } });
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));

  expect(await screen.findByText(/Required fields are marked/)).toBeTruthy();
});

test("tokenInput renders the help link and opens it via openUrl", async () => {
  detailData = detailFixture({ auth: tokenAuthInfo });
  beginData = beginResult({ authKind: "token" });
  await renderWizard();

  fireEvent.click(screen.getByRole("button", { name: /Get a token at/ }));
  expect(openUrl).toHaveBeenCalledWith("https://notion.example/help");
});

test("manualClientId shows dcrError, saves the id, and re-begin starts the browser flow", async () => {
  beginData = beginResult({ authKind: "oauth", needsClientId: true, dcrError: "registration rejected" });
  await renderWizard();

  expect(await screen.findByText("registration rejected")).toBeTruthy();

  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  fireEvent.change(screen.getByPlaceholderText("Paste the client ID from the vendor's console"), {
    target: { value: "client-abc" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));

  await waitFor(() => expect(setPluginOauthClientId).toHaveBeenCalledWith(LOCAL_RUNNER, "notion", "client-abc"));
  await waitFor(() => expect(beginPluginInstall).toHaveBeenCalledTimes(2));
  expect(await screen.findByText("Browser opened — finish signing in there.")).toBeTruthy();
});

test("manualClientId disables Continue until a client id is typed", async () => {
  beginData = beginResult({ authKind: "oauth", needsClientId: true });
  await renderWizard();

  const cont = screen.getByRole("button", { name: "Continue" }) as HTMLButtonElement;
  expect(cont.disabled).toBe(true);
});

test("manualClientId toasts and stays put with the typed value when saving the client id fails", async () => {
  beginData = beginResult({ authKind: "oauth", needsClientId: true });
  await renderWizard();

  const input = screen.getByPlaceholderText("Paste the client ID from the vendor's console") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "client-abc" } });

  setPluginOauthClientId.mockImplementationOnce(() =>
    Promise.resolve({ status: "error" as const, error: { message: "client id rejected" } }),
  );
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));

  await waitFor(() => expect(toastError).toHaveBeenCalledWith("client id rejected"));
  expect(beginPluginInstall).toHaveBeenCalledTimes(1);
  expect(screen.getByText(/doesn't support automatic app registration/)).toBeTruthy();
  expect(input.value).toBe("client-abc");
});

test("manualClientId toasts and stays put with no dead end when the re-begin call errors", async () => {
  beginData = beginResult({ authKind: "oauth", needsClientId: true });
  await renderWizard();

  const input = screen.getByPlaceholderText("Paste the client ID from the vendor's console") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "client-xyz" } });

  beginPluginInstall.mockImplementationOnce(() => Promise.resolve({ status: "error" as const, error: { message: "network unreachable" } }));
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));

  await waitFor(() => expect(setPluginOauthClientId).toHaveBeenCalledWith(LOCAL_RUNNER, "notion", "client-xyz"));
  await waitFor(() => expect(toastError).toHaveBeenCalledWith("network unreachable"));
  expect(screen.getByText(/doesn't support automatic app registration/)).toBeTruthy();
  expect(input.value).toBe("client-xyz");
});

test("manualClientId toasts and stays put when the re-begin succeeds but oauth is still unavailable (second DCR failure)", async () => {
  beginData = beginResult({ authKind: "oauth", needsClientId: true });
  await renderWizard();

  const input = screen.getByPlaceholderText("Paste the client ID from the vendor's console") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "client-def" } });

  beginData = beginResult({
    authKind: "oauth",
    needsClientId: true,
    oauthAvailable: false,
    dcrError: "still can't verify this client id",
  });
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));

  await waitFor(() => expect(beginPluginInstall).toHaveBeenCalledTimes(2));
  await waitFor(() => expect(toastError).toHaveBeenCalledWith("still can't verify this client id"));
  expect(screen.getByText(/doesn't support automatic app registration/)).toBeTruthy();
  expect(await screen.findByText("still can't verify this client id")).toBeTruthy();
  expect(input.value).toBe("client-def");
});

test("external oauth collects the client id and continues without a browser flow", async () => {
  detailData = detailFixture({ settings: [field("plugin.notion.user", "User")] });
  beginData = beginResult({ authKind: "oauth", oauthExternal: true, needsClientId: true });
  await renderWizard();

  fireEvent.change(screen.getByPlaceholderText("Paste the client ID from the vendor's console"), {
    target: { value: "google-client" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));

  await waitFor(() => expect(setPluginOauthClientId).toHaveBeenCalledWith(LOCAL_RUNNER, "notion", "google-client"));
  expect(await screen.findByText(/Required fields are marked/)).toBeTruthy();
  expect(beginPluginInstall).toHaveBeenCalledTimes(1);
});

test("pluginOauthCompletedMsg ok advances waitingOauth to done", async () => {
  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  await renderWizard();
  await waitFor(() => expect(pluginOauthCompletedMsgListen).toHaveBeenCalled());

  await act(async () => {
    completedListener?.({ payload: { pluginId: "notion", ok: true, error: null } });
  });

  expect(await screen.findByText("Notion is installed.")).toBeTruthy();
});

test("completion events for another plugin are ignored", async () => {
  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  await renderWizard();
  await waitFor(() => expect(pluginOauthCompletedMsgListen).toHaveBeenCalled());

  await act(async () => {
    completedListener?.({ payload: { pluginId: "other", ok: true, error: null } });
  });

  expect(screen.getByText("Browser opened — finish signing in there.")).toBeTruthy();
});

test("a failed completion shows an inline error, and paste-code completes via completePluginOauth", async () => {
  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  await renderWizard();
  await waitFor(() => expect(pluginOauthCompletedMsgListen).toHaveBeenCalled());

  await act(async () => {
    completedListener?.({ payload: { pluginId: "notion", ok: false, error: "sign-in timed out" } });
  });

  expect(await screen.findByText("sign-in timed out")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Retry" })).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Paste code instead" }));
  fireEvent.change(screen.getByPlaceholderText("Paste the code value from the callback URL"), {
    target: { value: "authcode-1" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Finish sign-in" }));

  await waitFor(() => expect(completePluginOauth).toHaveBeenCalledWith(LOCAL_RUNNER, "notion", "authcode-1", "state-123"));
  expect(await screen.findByText("Notion is installed.")).toBeTruthy();
});

// Phase 1 whole-branch review (HARD requirement): a successful manual paste
// must shut down the backend's pending loopback listener, or it leaks until
// the flow's own timeout.
test("a successful paste-code completion cancels the pending loopback listener", async () => {
  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  await renderWizard();

  fireEvent.click(screen.getByRole("button", { name: "Having trouble? Paste the code manually" }));
  fireEvent.change(screen.getByPlaceholderText("Paste the code value from the callback URL"), {
    target: { value: "authcode-2" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Finish sign-in" }));

  await waitFor(() => expect(completePluginOauth).toHaveBeenCalledWith(LOCAL_RUNNER, "notion", "authcode-2", "state-123"));
  await waitFor(() => expect(cancelPluginInstall).toHaveBeenCalledWith(LOCAL_RUNNER, "notion", "state-123"));
  expect(await screen.findByText("Notion is installed.")).toBeTruthy();
});

test("callbackMode manual shows the paste field by default with the port explanation", async () => {
  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, callbackMode: "manual", oauthBegin });
  await renderWizard();

  expect(screen.getByText(/Another sign-in is holding the callback port/)).toBeTruthy();
  expect(screen.getByPlaceholderText("Paste the code value from the callback URL")).toBeTruthy();
});

test("auto callback mode hides the paste field behind a Having trouble link", async () => {
  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  await renderWizard();

  expect(screen.queryByPlaceholderText("Paste the code value from the callback URL")).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Having trouble? Paste the code manually" }));
  expect(screen.getByPlaceholderText("Paste the code value from the callback URL")).toBeTruthy();
});

test("Reopen browser re-opens the authorize URL", async () => {
  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  await renderWizard();

  fireEvent.click(screen.getByRole("button", { name: "Reopen browser" }));
  expect(openUrl).toHaveBeenCalledWith("https://vendor.example.com/oauth/authorize?client_id=abc");
});

test("valueSet satisfies the required gate and optional unset fields never block", async () => {
  detailData = detailFixture({
    settings: [
      field("plugin.notion.app_key", "Application key", { secret: true, valueSet: true }),
      field("plugin.notion.site", "Site", { required: false }),
    ],
  });
  beginData = beginResult({ authKind: "none" });
  await renderWizard();

  const saved = screen.getByPlaceholderText("●●●● saved") as HTMLInputElement;
  expect(saved.value).toBe("");
  expect(saved.type).toBe("password");
  expect((screen.getByRole("button", { name: "Continue" }) as HTMLButtonElement).disabled).toBe(false);
  expect(screen.getByPlaceholderText("Optional — not set")).toBeTruthy();
});

test("a required unset setting disables Continue until typed, then saves on Continue", async () => {
  detailData = detailFixture({ settings: [field("plugin.notion.user", "User")] });
  beginData = beginResult({ authKind: "none" });
  await renderWizard();

  expect((screen.getByRole("button", { name: "Continue" }) as HTMLButtonElement).disabled).toBe(true);

  fireEvent.change(screen.getByPlaceholderText("Required — not set"), { target: { value: "alice" } });
  expect((screen.getByRole("button", { name: "Continue" }) as HTMLButtonElement).disabled).toBe(false);

  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  await waitFor(() => expect(setPluginSetting).toHaveBeenCalledWith(LOCAL_RUNNER, "plugin.notion.user", "alice"));
  expect(await screen.findByText("Notion is installed.")).toBeTruthy();
});

test("untouched fields are not saved on Continue", async () => {
  detailData = detailFixture({
    settings: [
      field("plugin.notion.app_key", "Application key", { secret: true, valueSet: true }),
      field("plugin.notion.site", "Site", { required: false }),
    ],
  });
  beginData = beginResult({ authKind: "none" });
  await renderWizard();

  fireEvent.click(screen.getByRole("button", { name: "Continue" }));
  expect(await screen.findByText("Notion is installed.")).toBeTruthy();
  expect(setPluginSetting).not.toHaveBeenCalled();
});

test("done enables the plugin and reloads the plugins store", async () => {
  beginData = beginResult({ authKind: "none" });
  await renderWizard();

  expect(await screen.findByText("Notion is installed.")).toBeTruthy();
  await waitFor(() => expect(setPluginEnabled).toHaveBeenCalledWith(LOCAL_RUNNER, "notion", true));
  await waitFor(() => expect(listPlugins).toHaveBeenCalled());
  expect(screen.getByText("It's enabled and ready for your agents.")).toBeTruthy();
});

test("experimental plugins are not auto-enabled at done", async () => {
  detailData = detailFixture({ experimental: true });
  beginData = beginResult({ authKind: "none" });
  await renderWizard();

  expect(await screen.findByText(/enable it from the card when ready/)).toBeTruthy();
  expect(setPluginEnabled).not.toHaveBeenCalled();
  await waitFor(() => expect(listPlugins).toHaveBeenCalled());
});

test("a null detail (pluginDetail failed, begin still routed to done) shows neutral copy and skips enable", async () => {
  pluginDetail.mockImplementationOnce(() => Promise.resolve({ status: "error" as const, error: { message: "manifest read failed" } }));
  beginData = beginResult({ authKind: "none" });
  await renderWizard();

  expect(await screen.findByText("Notion is installed.")).toBeTruthy();
  expect(screen.getByText("You can enable it from the card.")).toBeTruthy();
  expect(setPluginEnabled).not.toHaveBeenCalled();
  await waitFor(() => expect(listPlugins).toHaveBeenCalled());
});

test("closing during an oauth flow cancels the pending install with the state token", async () => {
  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  await renderWizard();

  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(cancelPluginInstall).toHaveBeenCalledWith(LOCAL_RUNNER, "notion", "state-123"));
  expect(onClose).toHaveBeenCalled();
});

test("closing an oauth wizard while begin is still in flight cancels with a null token", async () => {
  detailData = detailFixture({ auth: oauthAuthInfo });
  beginPluginInstall.mockImplementationOnce(() => new Promise<never>(() => {}));
  await renderWizard();

  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(cancelPluginInstall).toHaveBeenCalledWith(LOCAL_RUNNER, "notion", null));
  expect(onClose).toHaveBeenCalled();
});

test("closing a non-oauth wizard never calls cancelPluginInstall", async () => {
  detailData = detailFixture({ settings: [field("plugin.notion.user", "User")] });
  beginData = beginResult({ authKind: "none" });
  await renderWizard();

  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(cancelPluginInstall).not.toHaveBeenCalled();
  expect(onClose).toHaveBeenCalled();
});

// Phase 2 whole-branch review (fast follow, non-blocking): unmounting the
// wizard without going through close() — e.g. sidebar navigation away from
// the modal's host route — must still tear down a pending oauth flow.
test("unmounting the wizard mid-oauth cancels the pending install", async () => {
  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  const { unmount } = await renderWizard();

  expect(await screen.findByText("Browser opened — finish signing in there.")).toBeTruthy();

  unmount();
  await waitFor(() => expect(cancelPluginInstall).toHaveBeenCalledWith(LOCAL_RUNNER, "notion", "state-123"));
});

// Phase 2 whole-branch review (fast follow, non-blocking): a failed Retry
// clears oauthBegin (submitCode's own guard requires begin.oauthBegin.stateToken),
// so the button must not sit enabled-but-inert once that happens.
test("Finish sign-in stays disabled when a failed Retry leaves oauthBegin null", async () => {
  beginData = beginResult({ authKind: "oauth", oauthAvailable: true, oauthBegin });
  await renderWizard();
  await waitFor(() => expect(pluginOauthCompletedMsgListen).toHaveBeenCalled());

  await act(async () => {
    completedListener?.({ payload: { pluginId: "notion", ok: false, error: "sign-in timed out" } });
  });

  fireEvent.click(screen.getByRole("button", { name: "Paste code instead" }));

  beginData = beginResult({ authKind: "oauth", oauthAvailable: false, dcrError: "still can't verify this client id" });
  fireEvent.click(screen.getByRole("button", { name: "Retry" }));
  await waitFor(() => expect(beginPluginInstall).toHaveBeenCalledTimes(2));

  fireEvent.change(screen.getByPlaceholderText("Paste the code value from the callback URL"), {
    target: { value: "authcode-3" },
  });

  const finish = screen.getByRole("button", { name: "Finish sign-in" }) as HTMLButtonElement;
  expect(finish.disabled).toBe(true);
});
