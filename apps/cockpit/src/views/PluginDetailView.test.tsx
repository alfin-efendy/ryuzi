import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { PluginDetail } from "@/bindings";

// The view fetches straight from `commands.pluginDetail` (bypassing the
// `usePlugins` list store, which only carries the flattened `PluginInfo`)
// and only touches the store for `setEnabled`/`load`, so mocking the Tauri
// IPC boundary is enough to drive every section.

const githubDetail: PluginDetail = {
  info: {
    id: "github",
    name: "GitHub",
    description: "Repos, issues, and pull requests via GitHub's official remote MCP server.",
    icon: "github",
    categories: ["vcs", "issues"],
    verified: true,
    experimental: false,
    enabled: false,
    source: "catalog",
    capabilities: ["connector"],
  },
  auth: {
    kind: "token",
    setting: "plugin.github.token",
    env: "GITHUB_PERSONAL_ACCESS_TOKEN",
    helpUrl: "https://github.com/settings/tokens",
    configured: false,
    oauthConnectAvailable: false,
    oauthConnectError: null,
    oauthTokenStored: false,
    oauthReconnectRequired: false,
  },
  settings: [],
  mcp: [{ name: "github", transport: "http", commandOrUrl: "https://api.githubcopilot.com/mcp/" }],
  models: [],
  menuLabel: "GitHub",
  homepage: "https://github.com/github/github-mcp-server",
  publisher: "GitHub (official)",
};

const ollamaDetail: PluginDetail = {
  info: {
    id: "ollama",
    name: "Ollama",
    description: "Local models via Ollama.",
    icon: "cpu",
    categories: ["model-provider"],
    verified: true,
    experimental: false,
    enabled: true,
    source: "builtin",
    capabilities: ["provider"],
  },
  auth: null,
  settings: [
    {
      key: "plugin.ollama.base_url",
      label: "Base URL",
      help: "Defaults to http://localhost:11434",
      secret: false,
      required: false,
      valueSet: false,
    },
  ],
  mcp: [],
  models: ["llama3", "mistral"],
  menuLabel: null,
  homepage: null,
  publisher: "Ollama (local)",
};

const sandboxDetail: PluginDetail = {
  info: {
    id: "vercel-sandbox",
    name: "Vercel Sandbox",
    description: "Docs-only entry — no MCP surface.",
    icon: "box",
    categories: ["sandbox"],
    verified: false,
    experimental: true,
    enabled: false,
    source: "catalog",
    capabilities: [],
  },
  auth: null,
  settings: [],
  mcp: [],
  models: [],
  menuLabel: null,
  homepage: "https://vercel.com/docs/vercel-sandbox",
  publisher: "Vercel (no MCP surface)",
};

const oauthDetail: PluginDetail = {
  info: {
    id: "acme-oauth",
    name: "Acme OAuth",
    description: "HTTP MCP plugin authenticated through OAuth.",
    icon: "shield",
    categories: ["issues"],
    verified: true,
    experimental: false,
    enabled: true,
    source: "catalog",
    capabilities: ["connector"],
  },
  auth: {
    kind: "oauth",
    setting: null,
    env: null,
    helpUrl: "https://acme.example.com/help",
    configured: false,
    oauthConnectAvailable: true,
    oauthConnectError: null,
    oauthTokenStored: false,
    oauthReconnectRequired: false,
  },
  settings: [],
  mcp: [{ name: "acme", transport: "http", commandOrUrl: "https://api.acme.example.com/mcp" }],
  models: [],
  menuLabel: "Acme",
  homepage: "https://acme.example.com",
  publisher: "Acme",
};

const ok = <T,>(data: T) => Promise.resolve({ status: "ok" as const, data });
const err = (message: string) => Promise.resolve({ status: "error" as const, error: { message } });

const pluginDetail = mock((id: string) => {
  if (id === "github") return ok(githubDetail);
  if (id === "ollama") return ok(ollamaDetail);
  if (id === "acme-oauth") return ok(oauthDetail);
  if (id === "vercel-sandbox") return ok(sandboxDetail);
  return err("unknown plugin");
});
const setPluginEnabled = mock((_id: string, _enabled: boolean) => ok(null));
const setPluginSetting = mock((_key: string, _value: string) => ok(null));
const beginPluginOauth = mock((_pluginId: string) =>
  ok({
    stateToken: "state-123",
    authorizeUrl: "https://acme.example.com/oauth/authorize?client_id=acme-client",
    redirectUri: "http://127.0.0.1:8976/plugin-oauth/acme-oauth/callback",
  }),
);
const completePluginOauth = mock((_pluginId: string, _code: string, _stateToken: string) => ok(oauthDetail.auth));
const disconnectPluginOauth = mock((_pluginId: string) => ok({ ...oauthDetail.auth, configured: false, oauthTokenStored: false }));
const listPlugins = mock(() => ok([]));
const openUrl = mock(async (_url: string) => {});
const pluginOauthAuthorizeUrlMsgListen = mock(
  async (_cb: (event: { payload: { pluginId: string; authorizeUrl: string } }) => void) => () => {},
);

mock.module("@/bindings", () => ({
  events: {
    pluginOauthAuthorizeUrlMsg: {
      listen: pluginOauthAuthorizeUrlMsgListen,
    },
  },
  commands: {
    pluginDetail,
    setPluginEnabled,
    setPluginSetting,
    beginPluginOauth,
    completePluginOauth,
    disconnectPluginOauth,
    listPlugins,
  },
}));
mock.module("@tauri-apps/plugin-opener", () => ({ openUrl }));

const { PluginDetailView } = await import("@/views/PluginDetailView");
const { usePlugins } = await import("@/store-plugins");

beforeEach(() => {
  pluginDetail.mockClear();
  setPluginEnabled.mockClear();
  setPluginSetting.mockClear();
  beginPluginOauth.mockClear();
  completePluginOauth.mockClear();
  disconnectPluginOauth.mockClear();
  pluginOauthAuthorizeUrlMsgListen.mockClear();
  listPlugins.mockClear();
  openUrl.mockClear();
  usePlugins.setState({ plugins: [], loaded: false });
});

afterEach(() => {
  cleanup();
  usePlugins.setState({ plugins: [], loaded: false });
});

test("renders identity, about, and category/status badges from the manifest detail", async () => {
  render(<PluginDetailView id="github" />);
  await screen.findByText("GitHub");

  expect(pluginDetail).toHaveBeenCalledWith("github");
  expect(screen.getByText("GitHub (official)")).toBeTruthy();
  expect(screen.getByText(/Repos, issues, and pull requests/)).toBeTruthy();
  expect(screen.getByText("Verified")).toBeTruthy();
  expect(screen.getByText("vcs")).toBeTruthy();
  expect(screen.getByText("issues")).toBeTruthy();
  expect(screen.getByText("https://github.com/github/github-mcp-server")).toBeTruthy();
});

test("shows Not configured for an unset credential, disables Save until typed, and saves through setPluginSetting", async () => {
  render(<PluginDetailView id="github" />);
  await screen.findByText("GitHub");

  expect(screen.getByText("Not configured")).toBeTruthy();
  expect(screen.getByText(/Falls back to the GITHUB_PERSONAL_ACCESS_TOKEN environment variable/)).toBeTruthy();

  const save = screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;
  expect(save.disabled).toBe(true);

  const input = screen.getByPlaceholderText("Required — not set") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "ghp_test123" } });
  expect((screen.getByRole("button", { name: "Save" }) as HTMLButtonElement).disabled).toBe(false);

  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(setPluginSetting).toHaveBeenCalledWith("plugin.github.token", "ghp_test123"));
  await waitFor(() => expect(pluginDetail).toHaveBeenCalledTimes(2));
});

test("opens the auth help link through the shared openUrl mechanism", async () => {
  render(<PluginDetailView id="github" />);
  await screen.findByText("GitHub");

  fireEvent.click(screen.getByRole("button", { name: "Help" }));
  expect(openUrl).toHaveBeenCalledWith("https://github.com/settings/tokens");
});

test("oauth plugins start Cockpit sign-in through beginPluginOauth", async () => {
  render(<PluginDetailView id="acme-oauth" />);
  await screen.findByText("Acme OAuth");

  fireEvent.click(screen.getByRole("button", { name: "Connect" }));
  await waitFor(() => expect(beginPluginOauth).toHaveBeenCalledWith("acme-oauth"));
});

test("lists MCP servers with their transport and endpoint", async () => {
  render(<PluginDetailView id="github" />);
  await screen.findByText("GitHub");

  expect(screen.getByText("MCP servers")).toBeTruthy();
  expect(screen.getByText("http")).toBeTruthy();
  expect(screen.getByText("https://api.githubcopilot.com/mcp/")).toBeTruthy();
});

test("renders a Models card listing every model for provider-capable plugins", async () => {
  render(<PluginDetailView id="ollama" />);
  await screen.findByText("Ollama");

  expect(screen.getByText("Models")).toBeTruthy();
  expect(screen.getByText("llama3")).toBeTruthy();
  expect(screen.getByText("mistral")).toBeTruthy();
  expect(screen.getByText("Base URL")).toBeTruthy();
  expect(screen.getByPlaceholderText("Optional — not set")).toBeTruthy();
});

test("omits the Models card for non-provider plugins", async () => {
  render(<PluginDetailView id="github" />);
  await screen.findByText("GitHub");

  expect(screen.queryByText("Models")).toBeNull();
});

test("disables the enable switch for experimental plugins", async () => {
  render(<PluginDetailView id="vercel-sandbox" />);
  await screen.findByText("Vercel Sandbox");

  expect(screen.getByText("Experimental")).toBeTruthy();
  const sw = screen.getByRole("switch", { name: "Enabled" });
  expect(sw.getAttribute("aria-checked")).toBe("false");

  fireEvent.click(sw);
  expect(setPluginEnabled).not.toHaveBeenCalled();
  expect(sw.getAttribute("aria-checked")).toBe("false");
});

test("shows a not-found state for an unknown plugin id", async () => {
  render(<PluginDetailView id="ghost" />);
  await waitFor(() => expect(pluginDetail).toHaveBeenCalledWith("ghost"));
  expect(await screen.findByText("Plugin not found.")).toBeTruthy();
});
