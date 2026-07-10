import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { PluginDetail, PluginFieldInfo, PluginInstallBeginResult } from "@/bindings";

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
    },
    auth: overrides.auth ?? null,
    settings: overrides.settings ?? [],
    mcp: [],
    models: [],
    menuLabel: null,
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

const pluginDetail = mock((_id: string) => ok(detailData));
const beginPluginInstall = mock((_pluginId: string) => ok(beginData));
const setPluginOauthClientId = mock((_pluginId: string, _clientId: string) => ok(null));
const cancelPluginInstall = mock((_pluginId: string, _stateToken: string | null) => ok(null));
const completePluginOauth = mock((_pluginId: string, _code: string, _stateToken: string) => ok(oauthAuthInfo));
const setPluginSetting = mock((_key: string, _value: string) => ok(null));
const setPluginEnabled = mock((_id: string, _enabled: boolean) => ok(null));
const listPlugins = mock(() => ok([]));
const openUrl = mock(async (_url: string) => {});

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
  },
}));
mock.module("@tauri-apps/plugin-opener", () => ({ openUrl }));

const { InstallWizardModal } = await import("./InstallWizardModal");
const { usePlugins } = await import("@/store-plugins");

const onClose = mock(() => {});

async function renderWizard() {
  render(<InstallWizardModal pluginId="notion" pluginName="Notion" pluginIcon={null} onClose={onClose} />);
  await act(async () => {});
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
  openUrl.mockClear();
  pluginOauthCompletedMsgListen.mockClear();
  onClose.mockClear();
  usePlugins.setState({ plugins: [], loaded: false });
});

afterEach(() => {
  cleanup();
  usePlugins.setState({ plugins: [], loaded: false });
});

test("calls beginPluginInstall on mount and shows the checking spinner", async () => {
  beginPluginInstall.mockImplementationOnce(() => new Promise<never>(() => {}));
  await renderWizard();

  expect(beginPluginInstall).toHaveBeenCalledWith("notion");
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

  await waitFor(() => expect(setPluginSetting).toHaveBeenCalledWith("plugin.notion.token", "secret-token"));
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

  await waitFor(() => expect(setPluginOauthClientId).toHaveBeenCalledWith("notion", "client-abc"));
  await waitFor(() => expect(beginPluginInstall).toHaveBeenCalledTimes(2));
  expect(await screen.findByText("Browser opened — finish signing in there.")).toBeTruthy();
});

test("external oauth collects the client id and continues without a browser flow", async () => {
  detailData = detailFixture({ settings: [field("plugin.notion.user", "User")] });
  beginData = beginResult({ authKind: "oauth", oauthExternal: true, needsClientId: true });
  await renderWizard();

  fireEvent.change(screen.getByPlaceholderText("Paste the client ID from the vendor's console"), {
    target: { value: "google-client" },
  });
  fireEvent.click(screen.getByRole("button", { name: "Continue" }));

  await waitFor(() => expect(setPluginOauthClientId).toHaveBeenCalledWith("notion", "google-client"));
  expect(await screen.findByText(/Required fields are marked/)).toBeTruthy();
  expect(beginPluginInstall).toHaveBeenCalledTimes(1);
});
