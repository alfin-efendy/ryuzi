import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor, within } from "@testing-library/react";
import type {
  CatalogEntry,
  CmdError,
  CodexResetCreditResult,
  ConnectionInfo,
  EndpointKeyInfo,
  EndpointStatusInfo,
  ModelRouteInfo,
  ModelRouteTargetCapability,
  Result,
  UsageSeries,
} from "@/bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

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
  quotaCapability: null,
  models: ["gpt-4.1", "o3"],
  needsRelogin: false,
};

const secondConnection: ConnectionInfo = {
  ...connection,
  id: "c2",
  label: "Personal OpenAI",
  priority: 1,
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
  quotaCapability: "claude",
  models: ["claude-opus-4-8"],
  needsRelogin: false,
};

// Cloudflare Workers AI ids carry slashes ("@cf/meta/llama-3.1-8b-instruct")
// — the route target adapter must split at the FIRST slash only, keeping
// the model portion (which itself contains slashes) intact.
const cloudflareConnection: ConnectionInfo = {
  id: "c5",
  provider: "cloudflare-ai",
  providerName: "Cloudflare Workers AI",
  color: "#F38020",
  initial: "C",
  authType: "api_key",
  label: "Cloudflare",
  priority: 0,
  enabled: true,
  quotaCapability: null,
  models: ["@cf/meta/llama-3.1-8b-instruct"],
  needsRelogin: false,
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
  quotaCapability: null,
  models: ["claude-sonnet-4-5"],
  needsRelogin: false,
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
    usesDeviceGrant: false,
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
    usesDeviceGrant: false,
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
    usesDeviceGrant: false,
  },
];

const routes: ModelRouteInfo[] = [
  {
    id: "r1",
    name: "smart",
    enabled: true,
    strategy: "fallback",
    targets: [{ provider: "openai", model: "gpt-4.1", effort: null }],
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

const routeTargetCapabilities: ModelRouteTargetCapability[] = [
  {
    provider: "openai",
    model: "gpt-4.1",
    supported: [
      { value: "low", label: "Low", description: null },
      { value: "high", label: "High", description: null },
    ],
    providerDefault: null,
  },
  { provider: "openai", model: "o3", supported: [{ value: "high", label: "High", description: null }], providerDefault: null },
];
const historicalEffortCapabilities: ModelRouteTargetCapability[] = [
  { ...routeTargetCapabilities[0]!, supported: [{ value: "low", label: "Low", description: null }] },
  routeTargetCapabilities[1]!,
];

const saveModelRoute = mock((_runnerId: string, _route: ModelRouteInfo) => Promise.resolve({ status: "ok" as const, data: routes }));
const deleteModelRoute = mock((_runnerId: string, _id: string) => Promise.resolve({ status: "ok" as const, data: [] }));
const revokeEndpointKey = mock((_runnerId: string, _id: string) => Promise.resolve({ status: "ok" as const, data: [] }));

const refreshProviderModels = mock((_runnerId: string, _family: string) =>
  Promise.resolve({
    status: "ok" as const,
    data: [
      { connectionId: "c1", label: "Work OpenAI", ok: false, message: "Work OpenAI: model list request for openai failed with status 401" },
    ],
  }),
);

const renameConnection = mock((_runnerId: string, _id: string, label: string) =>
  Promise.resolve({ status: "ok" as const, data: [{ ...connection, label }, secondConnection] }),
);
const setConnectionEnabled = mock((_runnerId: string, _id: string, enabled: boolean) =>
  Promise.resolve({ status: "ok" as const, data: [{ ...connection, enabled }, secondConnection] }),
);
const removeConnection = mock((_runnerId: string, _id: string) => Promise.resolve({ status: "ok" as const, data: [secondConnection] }));
const reconnectOauth = mock((_runnerId: string, _id: string) => Promise.resolve({ status: "ok" as const, data: [claudeConnection] }));
const resetCodexCredit = mock(
  (_runnerId: string, _id: string): Promise<Result<CodexResetCreditResult, CmdError>> =>
    Promise.resolve({
      status: "ok" as const,
      data: { reset: true, code: null, windowsReset: 1, message: null, redeemRequestId: null },
    }),
);

// Mock the Tauri IPC boundary before the component (and the stores it uses) load.
mock.module("@/bindings", () => ({
  commands: {
    endpointStatus: () => Promise.resolve({ status: "ok", data: status }),
    listEndpointKeys: () => Promise.resolve({ status: "ok", data: keys }),
    listProviderCatalog: () => Promise.resolve({ status: "ok", data: catalog }),
    listConnections: () => Promise.resolve({ status: "ok", data: [connection, secondConnection] }),
    listModelRoutes: () => Promise.resolve({ status: "ok", data: routes }),
    listModelRouteTargetCapabilities: () => Promise.resolve({ status: "ok", data: routeTargetCapabilities }),
    projectRuntimeInfo: (projectId: string) =>
      Promise.resolve({
        status: "ok" as const,
        data: {
          projectId,
          model: null,
          storedEffort: null,
          effectiveEffort: null,
          effectiveEffortLabel: null,
          effectiveSource: "none",
          storedEffortStatus: "none",
          modelInfo: null,
        },
      }),
    listAgents: () =>
      Promise.resolve({ status: "ok", data: { agents: [], defaultAgentId: "", subagentModel: { kind: "route", route: "smart" } } }),
    listSelectableModels: () => Promise.resolve({ status: "ok", data: [] }),
    saveModelRoute,
    refreshProviderModels,
    deleteModelRoute,
    revokeEndpointKey,
    providerAccountRoute: (provider: string) => Promise.resolve({ status: "ok", data: { provider, strategy: "fallback" } }),
    setProviderAccountRoute: (provider: string, strategy: string) => Promise.resolve({ status: "ok", data: { provider, strategy } }),
    connectionUsage: () => Promise.resolve({ status: "ok", data: usage }),
    endpointUsage: () => Promise.resolve({ status: "ok", data: usage }),
    renameConnection,
    setConnectionEnabled,
    removeConnection,
    reconnectOauth,
    resetCodexCredit,
    moveConnection: () => Promise.resolve({ status: "ok", data: [secondConnection, connection] }),
    testConnection: () => Promise.resolve({ status: "ok", data: { ok: true, message: "Connection OK" } }),
    testConnectionModel: () => Promise.resolve({ status: "ok", data: { ok: true, status: "valid", message: "Model OK" } }),
    listModelStatuses: () => Promise.resolve({ status: "ok", data: [] }),
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
const { useEndpoint } = await import("@/store-endpoint");
const { useConnections } = await import("@/store-connections");
const { useModelRoutes } = await import("@/store-model-routes");
const { useUsage } = await import("@/store-usage");
const { useNav } = await import("@/store-nav");
const { useAgents } = await import("@/store-agents");

// The zustand singletons are shared across test files in one bun process, so
// reset BEFORE each test too — an earlier file's hydration (with its own
// fixtures) would otherwise satisfy the `loaded` guard and skip ours.
function resetStores() {
  useEndpoint.setState({ status: null, keys: [], loaded: false });
  useConnections.setState({ catalog: [], connections: [], loaded: false });
  useModelRoutes.setState({ routes: [], targetCapabilities: [], loaded: false });
  useUsage.setState({ byConnection: {}, endpoint: null });
  useNav.setState({ history: { back: [], current: { kind: "models" }, forward: [] } });
  useAgents.setState({ models: [], loaded: false });
}

beforeEach(() => {
  deleteModelRoute.mockClear();
  revokeEndpointKey.mockClear();
  resetStores();
});

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
  expect(screen.queryByText(/active$/i)).toBeNull();
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

test("provider rows show plain account and model counts without category or active badges", async () => {
  useConnections.setState({ catalog, connections: [], loaded: true });
  render(<ModelsView />);

  const anthropicRow = await screen.findByRole("button", { name: "Anthropic No accounts 2 catalog models" });
  expect(within(anthropicRow).getByText("No accounts · 2 catalog models")).toBeTruthy();
  expect(within(anthropicRow).queryByText("API key")).toBeNull();
  expect(within(anthropicRow).queryByText("OAuth")).toBeNull();

  const openaiRow = screen.getByRole("button", { name: "OpenAI No accounts 2 catalog models" });
  expect(within(openaiRow).getByText("No accounts · 2 catalog models")).toBeTruthy();
  expect(within(openaiRow).queryByText("API key")).toBeNull();
  expect(within(openaiRow).queryByText("OAuth")).toBeNull();
});

test("free-tier and device providers do not render pricing badges", async () => {
  useConnections.setState({
    catalog: [
      ...catalog,
      {
        id: "openrouter",
        name: "OpenRouter",
        family: "openrouter",
        color: "#6E56CF",
        initial: "R",
        category: "api_key",
        format: "openai",
        requiresBaseUrl: false,
        models: [],
        freeTier: true,
        riskNotice: false,
        usesDeviceGrant: false,
      },
      {
        id: "kiro",
        name: "Kiro (free tier)",
        family: "kiro",
        color: "#7C3AED",
        initial: "K",
        category: "device",
        format: "openai",
        requiresBaseUrl: false,
        models: ["claude-sonnet-5"],
        freeTier: false,
        riskNotice: true,
        usesDeviceGrant: false,
      },
    ],
    connections: [],
    loaded: true,
  });
  render(<ModelsView />);

  const openrouterRow = await screen.findByRole("button", { name: /OpenRouter/ });
  expect(within(openrouterRow).queryByText("Free tier")).toBeNull();
  const kiroRow = screen.getByRole("button", { name: /Kiro/ });
  expect(within(kiroRow).queryByText("Free")).toBeNull();
  expect(within(kiroRow).queryByText("device")).toBeNull();
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

test("revoke endpoint key uses the shared confirmation modal", async () => {
  render(<ModelsView />);
  fireEvent.click(await screen.findByRole("button", { name: "Endpoint" }));
  const trigger = await screen.findByRole("button", { name: "Revoke" });
  fireEvent.click(trigger);
  const dialog = screen.getByRole("dialog", { name: "Revoke API key?" });
  expect(dialog.querySelector('[data-slot="modal-footer"]')).toBeTruthy();
  expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
  fireEvent.click(within(dialog).getByRole("button", { name: "Revoke" }));
  await waitFor(() => expect(revokeEndpointKey).toHaveBeenCalledWith(LOCAL_RUNNER, "k1"));
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
  expect(screen.getByRole("button", { name: "Rename Work OpenAI" })).toBeTruthy();
  expect(screen.getByRole("switch", { name: "Enabled Work OpenAI" })).toBeTruthy();
  expect(screen.getByRole("switch", { name: "Enabled Personal OpenAI" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Add account" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Move Work OpenAI down" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Test Work OpenAI" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Delete Work OpenAI" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Delete Personal OpenAI" })).toBeTruthy();
  expect(screen.queryByTitle("Details")).toBeNull();
  expect(screen.queryByText(/sk-…3fk9/)).toBeNull();
  expect(screen.getByText("Usage")).toBeTruthy();
  expect(screen.getAllByText("Models").length).toBeGreaterThan(0);
  expect(screen.getByText("gpt-4.1")).toBeTruthy();
});

test("provider detail reads dynamic effort metadata from the agent store", async () => {
  useConnections.setState({ catalog, connections: [connection], loaded: true });
  useAgents.setState({
    models: [
      {
        kind: "concrete",
        requestValue: "openai/gpt-4.1",
        displayName: "GPT-4.1",
        preferenceKey: { family: "openai", model: "gpt-4.1" },
        supported: [
          { value: "low", label: "Low", description: null },
          { value: "high", label: "High", description: null },
        ],
        configuredDefault: null,
        resolvedDefault: "high",
        defaultSource: "provider",
      },
    ],
    loaded: true,
  });
  render(<ProviderDetailView provider="openai" />);

  expect(await screen.findByRole("combobox", { name: "Default effort for GPT-4.1" })).toBeTruthy();
});

test("changing account routing opens a listbox and persists the picked strategy", async () => {
  useConnections.setState({ catalog, connections: [connection, secondConnection], loaded: true });
  render(<ProviderDetailView provider="openai" />);

  await screen.findByText("2 active · By order");
  fireEvent.click(screen.getByRole("combobox", { name: "Account routing" }));
  fireEvent.click(await screen.findByRole("option", { name: "Round robin" }));

  expect(await screen.findByText("2 active · Round robin")).toBeTruthy();
});

test("Refresh models surfaces per-connection failures inline", async () => {
  refreshProviderModels.mockClear();
  useConnections.setState({ catalog, connections: [connection], loaded: true });
  render(<ProviderDetailView provider="openai" />);

  fireEvent.click(await screen.findByRole("button", { name: "Refresh models" }));

  await waitFor(() => expect(refreshProviderModels).toHaveBeenCalledWith(LOCAL_RUNNER, "openai"));
  expect(await screen.findByText("Work OpenAI: model list request for openai failed with status 401")).toBeTruthy();
});

test("provider detail spans the vendor family across catalog auth methods", async () => {
  useConnections.setState({ catalog, connections: [claudeConnection, anthropicApiConnection], loaded: true });
  render(<ProviderDetailView provider="anthropic" />);

  expect(screen.getByRole("heading", { level: 2, name: "Anthropic" })).toBeTruthy();
  expect(await screen.findByText("2 accounts · 2 catalog models")).toBeTruthy();
  expect(screen.getByText("Claude subscription")).toBeTruthy();
  expect(screen.getByText("Team Anthropic")).toBeTruthy();
  expect(screen.queryByText(/Subscription ·/)).toBeNull();
  expect(screen.queryByText(/API key ·/)).toBeNull();
});

test("rename pencil opens the rename modal and persists only the trimmed name", async () => {
  renameConnection.mockClear();
  useConnections.setState({ catalog, connections: [connection], loaded: true });
  render(<ProviderDetailView provider="openai" />);

  fireEvent.click(screen.getByRole("button", { name: "Rename Work OpenAI" }));
  const input = screen.getByRole("textbox", { name: "Account name" });
  fireEvent.change(input, { target: { value: "  Primary  " } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));

  await waitFor(() => expect(renameConnection).toHaveBeenCalledWith(LOCAL_RUNNER, "c1", "Primary"));
  await waitFor(() => expect(screen.queryByRole("dialog", { name: "Rename account" })).toBeNull());
});

test("enabled switch uses the dedicated account command", async () => {
  setConnectionEnabled.mockClear();
  useConnections.setState({ catalog, connections: [connection], loaded: true });
  render(<ProviderDetailView provider="openai" />);
  fireEvent.click(screen.getByRole("switch", { name: "Enabled Work OpenAI" }));
  await waitFor(() => expect(setConnectionEnabled).toHaveBeenCalledWith(LOCAL_RUNNER, "c1", false));
});

test("delete uses confirmation and restores focus to the invoking Delete button when cancelled", async () => {
  useConnections.setState({ catalog, connections: [connection], loaded: true });
  render(<ProviderDetailView provider="openai" />);
  const trigger = screen.getByRole("button", { name: "Delete Work OpenAI" });
  trigger.focus();
  fireEvent.click(trigger);
  expect(screen.getByRole("dialog", { name: "Delete account?" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(document.activeElement).toBe(trigger));
  await act(async () => {
    await Promise.resolve();
  });
});

test("redirect OAuth reconnects in place while device sign-in reopens Add account for the family", async () => {
  reconnectOauth.mockClear();
  const redirect = { ...claudeConnection, needsRelogin: true };
  useConnections.setState({ catalog, connections: [redirect], loaded: true });
  const { unmount } = render(<ProviderDetailView provider="anthropic" />);
  fireEvent.click(screen.getByRole("button", { name: "Reconnect Claude subscription" }));
  await waitFor(() => expect(reconnectOauth).toHaveBeenCalledWith(LOCAL_RUNNER, "c3"));
  await waitFor(() => expect(screen.getByRole("button", { name: "Reconnect Claude subscription" })).toBeTruthy());
  unmount();

  const deviceCatalog: CatalogEntry[] = [
    {
      id: "kiro",
      name: "Kiro",
      family: "kiro",
      color: "#7C3AED",
      initial: "K",
      category: "device",
      format: "openai",
      requiresBaseUrl: false,
      models: [],
      freeTier: true,
      riskNotice: false,
      usesDeviceGrant: false,
    },
  ];
  const device = { ...connection, id: "kiro-1", provider: "kiro", providerName: "Kiro", authType: "oauth", label: "Kiro" };
  useConnections.setState({ catalog: deviceCatalog, connections: [device], loaded: true });
  render(<ProviderDetailView provider="kiro" />);
  fireEvent.click(screen.getByRole("button", { name: "Reconnect Kiro" }));
  expect(screen.getByRole("dialog", { name: "Add account" })).toBeTruthy();
  await act(async () => {
    await Promise.resolve();
  });
});

test("quota renders only from capability and Codex reset uses the shared confirmation", async () => {
  resetCodexCredit.mockClear();
  const codex = { ...connection, authType: "oauth", quotaCapability: "codex" as const };
  useConnections.setState({ catalog, connections: [codex], loaded: true });
  render(<ProviderDetailView provider="openai" />);
  fireEvent.click(await screen.findByRole("button", { name: "Reset credit for Work OpenAI" }));
  const dialog = screen.getByRole("dialog", { name: "Reset credit?" });
  fireEvent.click(within(dialog).getByRole("button", { name: "Reset credit" }));
  await waitFor(() => expect(resetCodexCredit).toHaveBeenCalledWith(LOCAL_RUNNER, "c1"));
});

test("pointer-opened reset confirmation restores focus to the exact reset button on Cancel", async () => {
  const codex = { ...connection, authType: "oauth", quotaCapability: "codex" as const };
  useConnections.setState({ catalog, connections: [codex], loaded: true });
  render(<ProviderDetailView provider="openai" />);
  const trigger = await screen.findByRole("button", { name: "Reset credit for Work OpenAI" });
  fireEvent.click(trigger);
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(document.activeElement).toBe(trigger));
});

test("programmatic reset open retains account context after false confirm and restores its trigger on Cancel", async () => {
  resetCodexCredit.mockClear();
  resetCodexCredit.mockImplementationOnce(async () => ({ status: "error", error: { message: "No credit" } }));
  const codex = { ...connection, authType: "oauth", quotaCapability: "codex" as const };
  useConnections.setState({ catalog, connections: [codex], loaded: true });
  render(<ProviderDetailView provider="openai" />);
  const trigger = await screen.findByRole("button", { name: "Reset credit for Work OpenAI" });
  act(() => trigger.click());

  const dialog = await screen.findByRole("dialog", { name: "Reset credit?" });
  expect(dialog.textContent).toContain("Work OpenAI");
  fireEvent.click(within(dialog).getByRole("button", { name: "Reset credit" }));
  await waitFor(() => expect(resetCodexCredit).toHaveBeenCalledWith(LOCAL_RUNNER, "c1"));
  expect(screen.getByRole("dialog", { name: "Reset credit?" }).textContent).toContain("Work OpenAI");
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(document.activeElement).toBe(trigger));
});

test("Route tab lists model route aliases and their ordered targets", async () => {
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));

  expect(await screen.findByText("smart")).toBeTruthy();
  expect(screen.getByText("By order")).toBeTruthy();
  expect(screen.getByText("OpenAI / gpt-4.1")).toBeTruthy();
  expect(screen.getByRole("button", { name: "New route" })).toBeTruthy();
});

test("delete route uses the shared confirmation modal", async () => {
  render(<ModelsView />);
  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  const trigger = await screen.findByTitle("Delete route");
  fireEvent.click(trigger);
  const dialog = screen.getByRole("dialog", { name: "Delete route?" });
  expect(dialog.querySelector('[data-slot="modal-footer"]')).toBeTruthy();
  expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
  fireEvent.click(within(dialog).getByRole("button", { name: "Delete route" }));
  await waitFor(() => expect(deleteModelRoute).toHaveBeenCalledWith(LOCAL_RUNNER, "r1"));
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
  // Grouped presentation: provider family as the group header, model-only labels.
  expect(await screen.findByRole("option", { name: "gpt-4.1" })).toBeTruthy();
  expect(screen.getByText("OpenAI")).toBeTruthy();
});

test("route target dropdown collapses multiple accounts of the same provider into one option per model", async () => {
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "New route" }));

  // connection and secondConnection are both `openai` accounts that both
  // serve gpt-4.1 — the dropdown must dedupe to a single family+model option.
  fireEvent.click(screen.getByRole("combobox", { name: "Target 1" }));
  expect(await screen.findAllByRole("option", { name: "gpt-4.1" })).toHaveLength(1);
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
  expect(await screen.findAllByRole("option", { name: sharedModel })).toHaveLength(1);
  expect(screen.getByText("Anthropic")).toBeTruthy();
});

test("route target effort picker exposes only model default and exact capability options", async () => {
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  expect(screen.queryByText(/override/)).toBeNull();
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));
  const effort = await screen.findByRole("combobox", { name: "Target 1 effort" });
  fireEvent.click(effort);

  expect((await screen.findAllByRole("option")).map((option) => option.textContent)).toEqual(["Model default", "Low", "High"]);
  expect(screen.getByRole("option", { name: "Model default" })).toBeTruthy();
  expect(screen.getByRole("option", { name: "Low" })).toBeTruthy();
  expect(screen.getByRole("option", { name: "High" })).toBeTruthy();
});

test("route target without exact supported capability has no effort picker", async () => {
  useConnections.setState({ catalog, connections: [connection, anthropicApiConnection], loaded: true });
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "New route" }));
  fireEvent.click(screen.getByRole("combobox", { name: "Target 1" }));
  fireEvent.click(await screen.findByRole("option", { name: "claude-sonnet-4-5" }));

  expect(screen.queryByRole("combobox", { name: "Target 1 effort" })).toBeNull();
});

test("route target model change preserves compatible effort and clears incompatible effort", async () => {
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));
  const effort = await screen.findByRole("combobox", { name: "Target 1 effort" });
  fireEvent.click(effort);
  fireEvent.click(await screen.findByRole("option", { name: "High" }));

  const target = screen.getByRole("combobox", { name: "Target 1" });
  fireEvent.click(target);
  fireEvent.click(await screen.findByRole("option", { name: "o3" }));
  expect(screen.getByRole("combobox", { name: "Target 1 effort" }).textContent).toContain("High");

  fireEvent.click(screen.getByRole("combobox", { name: "Target 1" }));
  fireEvent.click(await screen.findByRole("option", { name: "gpt-4.1" }));
  fireEvent.click(screen.getByRole("combobox", { name: "Target 1 effort" }));
  fireEvent.click(await screen.findByRole("option", { name: "Low" }));
  fireEvent.click(screen.getByRole("combobox", { name: "Target 1" }));
  fireEvent.click(await screen.findByRole("option", { name: "o3" }));
  expect(screen.getByRole("combobox", { name: "Target 1 effort" }).textContent).toContain("Model default");
});

test("historical explicit target effort stays readable and can reset to model default", async () => {
  useModelRoutes.setState({
    routes: [{ ...routes[0], targets: [{ provider: "openai", model: "gpt-4.1", effort: "high" }] }],
    targetCapabilities: historicalEffortCapabilities,
    loaded: true,
  });
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  expect(screen.getByText("High override")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));
  const effort = await screen.findByRole("combobox", { name: "Target 1 effort" });
  expect(effort.textContent).toContain("high");
  fireEvent.click(effort);
  expect(screen.getByRole("option", { name: "Model default" })).toBeTruthy();
  expect(screen.getByRole("option", { name: "Low" })).toBeTruthy();
  fireEvent.keyDown(effort, { key: "Escape" });
});

test("route target model default saves null and cards summarize only explicit overrides", async () => {
  saveModelRoute.mockClear();
  useModelRoutes.setState({
    routes: [{ ...routes[0], targets: [{ provider: "openai", model: "gpt-4.1", effort: "high" }] }],
    targetCapabilities: routeTargetCapabilities,
    loaded: true,
  });
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  expect(screen.getByText("High override")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Edit" }));
  const effort = await screen.findByRole("combobox", { name: "Target 1 effort" });
  fireEvent.click(effort);
  fireEvent.click(await screen.findByRole("option", { name: "Model default" }));
  fireEvent.click(screen.getByRole("button", { name: "Save route" }));

  await waitFor(() => expect(saveModelRoute).toHaveBeenCalled());
  const [, savedRoute] = saveModelRoute.mock.calls[0] as [string, ModelRouteInfo];
  expect(savedRoute.targets[0]?.effort).toBeNull();
});

test("route form saves targets as {provider, model} scoped to the family, not the connection", async () => {
  saveModelRoute.mockClear();
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "New route" }));
  fireEvent.change(screen.getByPlaceholderText("smart"), { target: { value: "combo" } });
  fireEvent.click(screen.getByRole("button", { name: "Save route" }));

  await waitFor(() => expect(saveModelRoute).toHaveBeenCalled());
  const [, savedRoute] = saveModelRoute.mock.calls[0] as [string, ModelRouteInfo];
  expect(savedRoute.targets).toEqual([{ provider: "openai", model: "gpt-4.1", effort: null }]);
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

test("route target picker always shows the search input", async () => {
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "New route" }));
  fireEvent.click(screen.getByRole("combobox", { name: "Target 1" }));
  expect(await screen.findByPlaceholderText("Search…")).toBeTruthy();
});

test("route target adapter round-trips a slash-containing model id (cloudflare-ai)", async () => {
  saveModelRoute.mockClear();
  useConnections.setState({
    catalog: [
      ...catalog,
      {
        id: "cloudflare-ai",
        name: "Cloudflare Workers AI",
        family: "cloudflare-ai",
        color: "#F38020",
        initial: "C",
        category: "api_key",
        format: "openai",
        requiresBaseUrl: false,
        models: ["@cf/meta/llama-3.1-8b-instruct"],
        freeTier: false,
        riskNotice: false,
        usesDeviceGrant: false,
      },
    ],
    connections: [connection, cloudflareConnection],
    loaded: true,
  });
  render(<ModelsView />);

  fireEvent.click(await screen.findByRole("button", { name: "Route" }));
  fireEvent.click(screen.getByRole("button", { name: "New route" }));
  fireEvent.change(screen.getByPlaceholderText("smart"), { target: { value: "cf-route" } });

  fireEvent.click(screen.getByRole("combobox", { name: "Target 1" }));
  fireEvent.click(await screen.findByRole("option", { name: "@cf/meta/llama-3.1-8b-instruct" }));

  fireEvent.click(screen.getByRole("button", { name: "Save route" }));

  await waitFor(() => expect(saveModelRoute).toHaveBeenCalled());
  const [, savedRoute] = saveModelRoute.mock.calls[0] as [string, ModelRouteInfo];
  expect(savedRoute.targets).toEqual([{ provider: "cloudflare-ai", model: "@cf/meta/llama-3.1-8b-instruct", effort: null }]);
});
