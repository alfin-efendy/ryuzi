import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { CatalogEntry, ConnectionInfo, EndpointKeyInfo, EndpointStatusInfo, ModelRouteInfo, UsageSeries } from "@/bindings";

const status: EndpointStatusInfo = {
  running: true,
  port: 8899,
  baseUrl: "http://127.0.0.1:8899/v1",
  autostart: false,
  keychainStatus: "ok",
};

const keys: EndpointKeyInfo[] = [{ id: "k1", name: "VS Code", key: "rz-live-abc123", createdAt: 1751500800000, lastUsedAt: null }];

const connection: ConnectionInfo = {
  id: "c1",
  provider: "openai",
  providerName: "OpenAI",
  color: "#10A37F",
  initial: "O",
  authType: "api_key",
  label: "Work OpenAI",
  priority: 0,
  enabled: true,
  baseUrl: null,
  models: ["gpt-4.1", "o3"],
  keyMasked: "sk-…3fk9",
  needsRelogin: false,
  claudeCloaking: false,
};

const secondConnection: ConnectionInfo = {
  ...connection,
  id: "c2",
  label: "Personal OpenAI",
  priority: 1,
  keyMasked: "sk-…zz99",
  models: ["gpt-4.1"],
};

const claudeConnection: ConnectionInfo = {
  id: "c3",
  provider: "anthropic-oauth",
  providerName: "Claude Code",
  color: "#D97757",
  initial: "C",
  authType: "oauth",
  label: "Claude subscription",
  priority: 0,
  enabled: true,
  baseUrl: null,
  models: ["claude-opus-4-8"],
  keyMasked: null,
  needsRelogin: false,
  claudeCloaking: true,
};

const anthropicApiConnection: ConnectionInfo = {
  id: "c4",
  provider: "anthropic",
  providerName: "Anthropic",
  color: "#D97757",
  initial: "A",
  authType: "api_key",
  label: "Team Anthropic",
  priority: 0,
  enabled: true,
  baseUrl: null,
  models: ["claude-sonnet-4-5"],
  keyMasked: "sk-…9f21",
  needsRelogin: false,
  claudeCloaking: false,
};

const catalog: CatalogEntry[] = [
  {
    id: "openai",
    name: "OpenAI",
    family: "openai",
    color: "#10A37F",
    initial: "O",
    category: "api_key",
    format: "openai",
    requiresBaseUrl: false,
    models: ["gpt-4.1", "o3"],
    freeTier: false,
    riskNotice: false,
  },
  {
    id: "anthropic",
    name: "Anthropic",
    family: "anthropic",
    color: "#D97757",
    initial: "A",
    category: "api_key",
    format: "anthropic",
    requiresBaseUrl: false,
    models: ["claude-sonnet-4-5"],
    freeTier: false,
    riskNotice: false,
  },
  {
    id: "anthropic-oauth",
    name: "Claude Code",
    family: "anthropic",
    color: "#D97757",
    initial: "A",
    category: "oauth",
    format: "anthropic",
    requiresBaseUrl: false,
    models: ["claude-opus-4-8"],
    freeTier: false,
    riskNotice: false,
  },
];

const routes: ModelRouteInfo[] = [
  {
    id: "r1",
    name: "smart",
    enabled: true,
    strategy: "fallback",
    targets: [{ provider: "openai", model: "gpt-4.1" }],
    createdAt: 1751500800000,
    updatedAt: 1751500800000,
  },
];

const usage: UsageSeries = {
  days: [
    { day: "2026-07-03", requests: 4, inputTokens: 1200, outputTokens: 300 },
    { day: "2026-07-04", requests: 3, inputTokens: 900, outputTokens: 210 },
  ],
  todayRequests: 3,
  todayInputTokens: 900,
  todayOutputTokens: 210,
};

const saveModelRoute = mock((_route: ModelRouteInfo) => Promise.resolve({ status: "ok" as const, data: routes }));

const updateConnection = mock(
  (
    _id: string,
    _label: string,
    _enabled: boolean,
    _apiKey: string | null,
    _baseUrl: string | null,
    _models: string[],
    _claudeCloaking: boolean | null,
  ) => Promise.resolve({ status: "ok" as const, data: [connection, secondConnection, claudeConnection] }),
);

// Mock the Tauri IPC boundary before the component (and the stores it uses) load.
mock.module("@/bindings", () => ({
  commands: {
    endpointStatus: () => Promise.resolve({ status: "ok", data: status }),
    listEndpointKeys: () => Promise.resolve({ status: "ok", data: keys }),
    listProviderCatalog: () => Promise.resolve({ status: "ok", data: catalog }),
    listConnections: () => Promise.resolve({ status: "ok", data: [connection, secondConnection] }),
    listModelRoutes: () => Promise.resolve({ status: "ok", data: routes }),
    saveModelRoute,
    deleteModelRoute: (_id: string) => Promise.resolve({ status: "ok", data: [] }),
    providerAccountRoute: (provider: string) => Promise.resolve({ status: "ok", data: { provider, strategy: "fallback" } }),
    setProviderAccountRoute: (provider: string, strategy: string) => Promise.resolve({ status: "ok", data: { provider, strategy } }),
    connectionUsage: () => Promise.resolve({ status: "ok", data: usage }),
    endpointUsage: () => Promise.resolve({ status: "ok", data: usage }),
    updateConnection,
    moveConnection: () => Promise.resolve({ status: "ok", data: [secondConnection, connection] }),
    testConnection: () => Promise.resolve({ status: "ok", data: { ok: true, message: "Connection OK" } }),
    testConnectionModel: () => Promise.resolve({ status: "ok", data: { ok: true, message: "Model OK" } }),
    connectionProviderQuota: () =>
      Promise.resolve({
        status: "ok",
        data: {
          provider: "anthropic-oauth",
          plan: "Claude Code",
          message: null,
          limitReached: false,
          reviewLimitReached: false,
          resetCredits: null,
          quotas: [],
        },
      }),
  },
  events: {
    oauthAuthorizeUrlMsg: {
      listen: () => Promise.resolve(() => {}),
    },
  },
}));

const { ModelsView } = await import("./ModelsView");
const { ProviderDetailView } = await import("./ProviderDetailView");
const { ConnectionDetailView } = await import("./ConnectionDetailView");
const { useEndpoint } = await import("@/store-endpoint");
const { useConnections } = await import("@/store-connections");
const { useModelRoutes } = await import("@/store-model-routes");
const { useUsage } = await import("@/store-usage");
const { useNav } = await import("@/store-nav");

// The zustand singletons are shared across test files in one bun process, so
// reset BEFORE each test too — an earlier file's hydration (with its own
// fixtures) would otherwise satisfy the `loaded` guard and skip ours.
function resetStores() {
  useEndpoint.setState({ status: null, keys: [], loaded: false });
  useConnections.setState({ catalog: [], connections: [], loaded: false });
  useModelRoutes.setState({ routes: [], loaded: false });
  useUsage.setState({ byConnection: {}, endpoint: null });
  useNav.setState({ history: { back: [], current: { kind: "models" }, forward: [] } });
}

beforeEach(resetStores);

afterEach(() => {
  cleanup();
  resetStores();
});

test("renders provider list first with tab order Providers, Route, Endpoint", async () => {
  render(<ModelsView />);

  await screen.findByRole("button", { name: "OpenAI 2 accounts 2 models" });
  expect(screen.getByRole("heading", { level: 2, name: "Models" })).toBeTruthy();
  const tabs = screen
    .getAllByRole("button")
    .map((button) => button.textContent?.trim())
    .filter(Boolean);
  expect(tabs.slice(0, 3)).toEqual(["Providers", "Route", "Endpoint"]);
  expect(screen.getByRole("button", { name: "OpenAI 2 accounts 2 models" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Anthropic No accounts 2 catalog models" })).toBeTruthy();
  expect(screen.queryByText("Work OpenAI")).toBeNull();
});

test("provider list has no global Add connection button", async () => {
  render(<ModelsView />);

  await screen.findByRole("button", { name: "OpenAI 2 accounts 2 models" });
  expect(screen.queryByRole("button", { name: /add connection/i })).toBeNull();
});

test("providers tab groups anthropic + anthropic-oauth accounts into one Anthropic row", async () => {
  useConnections.setState({
    catalog,
    connections: [connection, secondConnection, anthropicApiConnection, claudeConnection],
    loaded: true,
  });
  render(<ModelsView />);

  const anthropicRow = await screen.findByRole("button", { name: "Anthropic 2 accounts 2 models" });
  expect(anthropicRow).toBeTruthy();
  expect(screen.queryByRole("button", { name: /add connection/i })).toBeNull();

  fireEvent.click(anthropicRow);
  expect(useNav.getState().history.current).toEqual({ kind: "providerDetail", provider: "anthropic" });
});

test("seeds the settings form from the hydrated endpoint status", async () => {
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Endpoint" }));
  expect(await screen.findByDisplayValue("8899")).toBeTruthy();
  const autostart = screen.getByRole("switch", { name: "Start automatically with Cockpit" });
  expect(autostart.getAttribute("aria-checked")).toBe("false");
  expect(screen.getByRole("button", { name: "Save" })).toBeTruthy();
});

test("lists endpoint API keys with revoke and create controls", async () => {
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Endpoint" }));
  await screen.findByText("VS Code");
  expect(screen.getByText("rz-live-abc123")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Revoke" })).toBeTruthy();
  const newKey = screen.getByRole("button", { name: "New key" }) as HTMLButtonElement;
  expect(newKey.disabled).toBe(true);
});

test("provider detail shows accounts for the selected provider", async () => {
  useConnections.setState({ catalog, connections: [connection, secondConnection], loaded: true });
  render(<ProviderDetailView provider="openai" />);

  expect(screen.getByRole("heading", { level: 2, name: "OpenAI" })).toBeTruthy();
  expect(await screen.findByText("2 active · By order")).toBeTruthy();
  expect(screen.getByText("Accounts")).toBeTruthy();
  expect(screen.getByText("Account routing")).toBeTruthy();
  expect(screen.getByRole("combobox", { name: "Account routing" })).toBeTruthy();
  expect(screen.getByText("Work OpenAI")).toBeTruthy();
  expect(screen.getByText("Personal OpenAI")).toBeTruthy();
  expect(screen.getAllByRole("switch", { name: "Enabled" })).toHaveLength(2);
  expect(screen.getByRole("button", { name: "Add account" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Move Work OpenAI down" })).toBeTruthy();
  expect(screen.getByText("Usage")).toBeTruthy();
  expect(screen.getAllByText("Models").length).toBeGreaterThan(0);
  expect(screen.getByText("gpt-4.1")).toBeTruthy();
});

test("changing account routing opens a listbox and persists the picked strategy", async () => {
  useConnections.setState({ catalog, connections: [connection, secondConnection], loaded: true });
  render(<ProviderDetailView provider="openai" />);

  await screen.findByText("2 active · By order");
  fireEvent.click(screen.getByRole("combobox", { name: "Account routing" }));
  fireEvent.click(await screen.findByRole("option", { name: "Round robin" }));

  expect(await screen.findByText("2 active · Round robin")).toBeTruthy();
});

test("provider detail spans the vendor family across catalog auth methods", async () => {
  useConnections.setState({ catalog, connections: [claudeConnection, anthropicApiConnection], loaded: true });
  render(<ProviderDetailView provider="anthropic" />);

  expect(screen.getByRole("heading", { level: 2, name: "Anthropic" })).toBeTruthy();
  expect(await screen.findByText("2 accounts · 2 catalog models")).toBeTruthy();
  expect(screen.getByText("Claude subscription")).toBeTruthy();
  expect(screen.getByText("Team Anthropic")).toBeTruthy();
  expect(screen.getByText("Subscription · no key · 1 model")).toBeTruthy();
  expect(screen.getByText("API key · sk-…9f21 · 1 model")).toBeTruthy();
});

test("Route tab lists model route aliases and their ordered targets", async () => {
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));

  expect(await screen.findByText("smart")).toBeTruthy();
  expect(screen.getByText("By order")).toBeTruthy();
  expect(screen.getByText("OpenAI / gpt-4.1")).toBeTruthy();
  expect(screen.getByRole("button", { name: "New route" })).toBeTruthy();
});

test("route form renders strategy and target comboboxes with option lists", async () => {
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  fireEvent.click(await screen.findByRole("button", { name: "Edit" }));

  const strategy = await screen.findByRole("combobox", { name: "Strategy" });
  fireEvent.click(strategy);
  expect(await screen.findByRole("option", { name: "Round robin" })).toBeTruthy();
  expect(screen.getByRole("option", { name: "By order" })).toBeTruthy();
  // close the strategy popup before opening the target one
  fireEvent.keyDown(strategy, { key: "Escape" });

  const target = screen.getByRole("combobox", { name: "Target 1" });
  fireEvent.click(target);
  expect(await screen.findByRole("option", { name: "OpenAI / gpt-4.1" })).toBeTruthy();
});

test("route target dropdown collapses multiple accounts of the same provider into one option per model", async () => {
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "New route" }));

  // connection and secondConnection are both `openai` accounts that both
  // serve gpt-4.1 — the dropdown must dedupe to a single family+model option.
  fireEvent.click(screen.getByRole("combobox", { name: "Target 1" }));
  expect(await screen.findAllByRole("option", { name: "OpenAI / gpt-4.1" })).toHaveLength(1);
});

test("route target dropdown collapses anthropic + anthropic-oauth accounts sharing a model into one Anthropic option", async () => {
  const sharedModel = "claude-4.5-haiku";
  useConnections.setState({
    catalog,
    connections: [
      { ...anthropicApiConnection, models: [sharedModel] },
      { ...claudeConnection, models: [sharedModel] },
    ],
    loaded: true,
  });
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "New route" }));

  fireEvent.click(screen.getByRole("combobox", { name: "Target 1" }));
  expect(await screen.findAllByRole("option", { name: `Anthropic / ${sharedModel}` })).toHaveLength(1);
});

test("route form saves targets as {provider, model} scoped to the family, not the connection", async () => {
  saveModelRoute.mockClear();
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "New route" }));
  fireEvent.change(screen.getByPlaceholderText("smart"), { target: { value: "combo" } });
  fireEvent.click(screen.getByRole("button", { name: "Save route" }));

  await waitFor(() => expect(saveModelRoute).toHaveBeenCalled());
  const [savedRoute] = saveModelRoute.mock.calls[0] as [ModelRouteInfo];
  expect(savedRoute.targets).toEqual([{ provider: "openai", model: "gpt-4.1" }]);
});

test("connection detail back returns to its provider detail", () => {
  useConnections.setState({ catalog, connections: [connection], loaded: true });
  render(<ConnectionDetailView id="c1" />);

  expect(screen.getByRole("button", { name: "OpenAI" })).toBeTruthy();
  expect(screen.queryByText("Usage")).toBeNull();
  expect(screen.queryByText("Models")).toBeNull();

  fireEvent.click(screen.getByRole("button", { name: "OpenAI" }));
  expect(useNav.getState().history.current).toEqual({ kind: "providerDetail", provider: "openai" });
});

test("connection detail saves the Claude cloaking toggle", async () => {
  updateConnection.mockClear();
  useConnections.setState({ catalog, connections: [claudeConnection], loaded: true });
  render(<ConnectionDetailView id="c3" />);

  const toggle = screen.getByRole("switch", { name: "Claude Code cloaking" });
  expect(toggle.getAttribute("aria-checked")).toBe("true");
  fireEvent.click(toggle);
  fireEvent.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() =>
    expect(updateConnection).toHaveBeenCalledWith("c3", "Claude subscription", true, null, null, ["claude-opus-4-8"], false),
  );
});

test("connection detail back button routes to the account's vendor family, not its raw catalog id", () => {
  useConnections.setState({ catalog, connections: [claudeConnection], loaded: true });
  render(<ConnectionDetailView id="c3" />);

  // The back button is labelled with the family head's catalog name
  // ("Anthropic"), not the member's own name ("Claude Code").
  fireEvent.click(screen.getByRole("button", { name: "Anthropic" }));
  expect(useNav.getState().history.current).toEqual({ kind: "providerDetail", provider: "anthropic" });
});

test("warns when secrets fall back to a local file instead of the OS keychain", async () => {
  useEndpoint.setState({ status: { ...status, keychainStatus: "fileFallback" }, keys, loaded: true });
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Endpoint" }));
  await screen.findByText("Secrets are stored in a local file, not the OS keychain.");
  expect(screen.queryByText("Secrets are stored unencrypted — no OS keychain available.")).toBeNull();
});

test("warns more strongly when no keychain or file fallback is available", async () => {
  useEndpoint.setState({ status: { ...status, keychainStatus: "unavailable" }, keys, loaded: true });
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Endpoint" }));
  await screen.findByText("Secrets are stored unencrypted — no OS keychain available.");
});

test("shows no keychain warning when the master key is in the OS keychain", async () => {
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Endpoint" }));
  await screen.findByText("Running on http://127.0.0.1:8899/v1");
  expect(screen.queryByText("Secrets are stored in a local file, not the OS keychain.")).toBeNull();
  expect(screen.queryByText("Secrets are stored unencrypted — no OS keychain available.")).toBeNull();
});

test("shows empty states for API keys, providers, and routes", async () => {
  useEndpoint.setState({ status, keys: [], loaded: true });
  useConnections.setState({ catalog: [], connections: [], loaded: true });
  useModelRoutes.setState({ routes: [], loaded: true });
  render(<ModelsView />);

  expect(await screen.findByText("No providers in the catalog yet.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Route" }));
  expect(screen.getByText("No routes yet. Create a route alias to expose a combo-style model.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Endpoint" }));
  expect(await screen.findByText("No API keys yet — create one for external tools.")).toBeTruthy();
});
