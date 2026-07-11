import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type {
  AppInfo,
  CatalogEntry,
  ConnectionInfo,
  EndpointKeyInfo,
  EndpointStatusInfo,
  RuntimeConfigStatusInfo,
  RuntimeInfo,
} from "@/bindings";

// Fixtures shaped after the generated Tauri bindings. The runtime carries an
// available update (latest !== installed) so the update banner renders.
const claudeRuntime: RuntimeInfo = {
  id: "claude",
  name: "Claude Code",
  color: "#D97757",
  initial: "C",
  connection: "Anthropic",
  binaryPath: "/usr/local/bin/claude",
  installedVersion: "2.0.1",
  latestVersion: "2.1.0",
  npmPackage: "@anthropic-ai/claude-code",
  models: ["claude-opus-4", "claude-sonnet-4"],
  selectableModels: [],
  enabled: true,
  model: "sonnet",
  permMode: "ask",
  flags: "",
  tiers: [
    { id: "smart", label: "Smart", value: "claude-opus-4", combo: false },
    { id: "fast", label: "Fast", value: null, combo: false },
  ],
  isDefault: false,
  runnable: true,
};

// Native runtime: in-process, no tiers; its models mix a bare route alias
// ("smart") with a family-prefixed connection model.
const nativeRuntime: RuntimeInfo = {
  ...claudeRuntime,
  id: "native",
  name: "Ryuzi",
  connection: "In-process",
  binaryPath: "in-process",
  installedVersion: "0.5.0",
  latestVersion: null,
  npmPackage: null,
  models: ["smart", "anthropic/claude-opus-4"],
  model: "",
  tiers: [],
  isDefault: true,
};

const githubApp: AppInfo = {
  id: "github",
  name: "GitHub",
  kind: "MCP · stdio",
  initial: "G",
  color: "#8B5CF6",
  desc: "GitHub MCP server",
  transport: "stdio",
  command: "npx",
  args: ["-y", "@modelcontextprotocol/server-github"],
  url: null,
  scope: "all",
  scopeGateways: [],
  status: "connected",
  statusDetail: null,
  version: "1.0.0",
  publisher: "GitHub",
  authKind: "none",
  authDetail: null,
  tools: [],
  agentAccess: [{ agentId: "claude", allowed: true }],
};

const anthropicConnection: ConnectionInfo = {
  id: "conn-1",
  provider: "anthropic",
  providerName: "Anthropic",
  color: "#D97757",
  initial: "A",
  authType: "apiKey",
  label: "Anthropic",
  priority: 0,
  enabled: true,
  quotaCapability: null,
  models: ["claude-sonnet-4", "claude-sonnet-4"],
  needsRelogin: false,
};

const endpointUp: EndpointStatusInfo = {
  running: true,
  port: 8787,
  baseUrl: "http://127.0.0.1:8787",
  autostart: false,
  keychainStatus: "ok",
};
const endpointKey: EndpointKeyInfo = { id: "key-1", name: "local", key: "rz-abc123", createdAt: 1, lastUsedAt: null };
const configStatus: RuntimeConfigStatusInfo = {
  configPath: "/home/user/.claude/settings.json",
  exists: true,
  configured: false,
  supported: true,
};

// Catalog with the anthropic family head so the grouped pickers can resolve
// bare ("claude-opus-4") and prefixed ("anthropic/…") model ids to a family.
const catalogEntries: CatalogEntry[] = [
  {
    id: "anthropic",
    name: "Anthropic",
    family: "anthropic",
    color: "#D97757",
    initial: "A",
    category: "api_key",
    format: "anthropic",
    requiresBaseUrl: false,
    models: ["claude-opus-4", "claude-sonnet-4"],
    freeTier: false,
    riskNotice: false,
    usesDeviceGrant: false,
  },
];

const ok = <T,>(data: T) => Promise.resolve({ status: "ok" as const, data });

const runtimeConfigStatus = mock((_id: string) => ok(configStatus));
const resetRuntimeConfig = mock((_id: string) => ok({ ...configStatus, configured: false }));
const updateRuntimeConfig = mock((_id: string, enabled: boolean, model: string | null, permMode: string, flags: string) =>
  ok([{ ...claudeRuntime, enabled, model: model ?? "", permMode, flags }]),
);

// Mock the Tauri IPC boundary before the component (and the stores it pulls
// in) resolve "@/bindings"; only mount-time commands need implementations.
mock.module("@/bindings", () => ({
  commands: {
    runtimeConfigStatus,
    resetRuntimeConfig,
    updateRuntimeConfig,
    endpointStatus: () => ok(endpointUp),
    listEndpointKeys: () => ok([endpointKey]),
    listProviderCatalog: () => ok(catalogEntries),
    listConnections: () => ok([anthropicConnection]),
    listModelRoutes: () => ok([]),
    listApps: () => ok([githubApp]),
    listRuntimes: () => ok([claudeRuntime]),
    refreshRuntimes: () => ok([claudeRuntime]),
  },
  events: {},
}));

const { RuntimeDetailView } = await import("./RuntimeDetailView");
const { useRuntimes } = await import("@/store-runtimes");
const { useApps } = await import("@/store-apps");
const { useEndpoint } = await import("@/store-endpoint");
const { useConnections } = await import("@/store-connections");
const { useModelRoutes } = await import("@/store-model-routes");

beforeEach(() => {
  runtimeConfigStatus.mockClear();
  resetRuntimeConfig.mockClear();
  updateRuntimeConfig.mockClear();
  useRuntimes.setState({ runtimes: [claudeRuntime], loaded: true, refreshing: false, updating: {}, updateLog: {} });
  useApps.setState({ apps: [githubApp], loaded: true, probing: null });
  useEndpoint.setState({ status: null, keys: [], loaded: false });
  useConnections.setState({ catalog: [], connections: [], loaded: false });
  useModelRoutes.setState({ routes: [], loaded: false });
});

// Reset the shared zustand singletons on the way out too: the view's mount
// hydrate marks them loaded with THIS file's fixtures, and a later test file
// in the same bun process would otherwise inherit that state.
afterEach(() => {
  cleanup();
  useRuntimes.setState({ runtimes: [], loaded: false, refreshing: false, updating: {}, updateLog: {} });
  useApps.setState({ apps: [], loaded: false, probing: null });
  useEndpoint.setState({ status: null, keys: [], loaded: false });
  useConnections.setState({ catalog: [], connections: [], loaded: false });
  useModelRoutes.setState({ routes: [], loaded: false });
});

// Render and flush the mount-effect hydrates (config status, endpoint,
// connections) inside act so their setState calls do not fire mid-assertion.
async function renderView(id = "claude") {
  render(<RuntimeDetailView id={id} />);
  await act(async () => {});
}

test("renders the runtime identity, enabled switch, and configuration landmarks", async () => {
  await renderView();

  expect(screen.getByText("Claude Code")).toBeTruthy();
  expect(screen.getByText("Anthropic · /usr/local/bin/claude")).toBeTruthy();
  expect(screen.getByText("Configuration")).toBeTruthy();
  expect(screen.getByText("Model mapping")).toBeTruthy();

  const enabled = screen.getByRole("switch", { name: "Enabled" });
  expect(enabled.getAttribute("aria-checked")).toBe("true");
  expect(screen.getByRole("button", { name: "Make default" })).toBeTruthy();
  // Permission-mode segmented control renders every option, with the current one described below it.
  for (const label of ["Plan", "Ask", "Edit", "Full"]) expect(screen.getByRole("button", { name: label })).toBeTruthy();
  expect(screen.getByText("Asks before edits and shell commands.")).toBeTruthy();
});

test("shows the update banner with the npm install command when a newer version exists", async () => {
  await renderView();

  expect(screen.getByText("Update available — 2.1.0 (installed 2.0.1)")).toBeTruthy();
  expect(screen.getByText("npm install -g @anthropic-ai/claude-code")).toBeTruthy();
  const updateNow = screen.getByRole("button", { name: "Update now" }) as HTMLButtonElement;
  expect(updateNow.disabled).toBe(false);
  expect(screen.getByRole("button", { name: "Copy command" })).toBeTruthy();
});

test("loads endpoint config status and enables Apply once server, key, and models are present", async () => {
  await renderView();

  expect(await screen.findByText("/home/user/.claude/settings.json")).toBeTruthy();
  expect(screen.getByText("Not configured")).toBeTruthy();
  expect(runtimeConfigStatus).toHaveBeenCalledWith("claude");

  // Claude gets Opus/Sonnet/Haiku tier pickers plus the default-model picker;
  // each is a Combobox whose popup offers the enabled connection's models.
  for (const name of ["Opus", "Sonnet", "Haiku", "Default model"]) {
    expect(await screen.findByRole("combobox", { name })).toBeTruthy();
  }
  fireEvent.click(screen.getByRole("combobox", { name: "Opus" }));
  // The provider-prefixed id ("anthropic/claude-sonnet-4") renders under its
  // family group with the prefix trimmed off the label; the value is unchanged.
  expect(await screen.findByRole("option", { name: "claude-sonnet-4" })).toBeTruthy();
  await waitFor(() => expect((screen.getByRole("button", { name: "Apply" }) as HTMLButtonElement).disabled).toBe(false));
});

test("tier picker groups runtime models by family with the combo sentinel first", async () => {
  await renderView();

  fireEvent.click(screen.getByRole("combobox", { name: "Smart model" }));
  const options = await screen.findAllByRole("option");
  // The sentinel is an ungrouped (headingless) leading item, before the groups.
  expect(options[0]?.textContent).toBe("Route by task (combo)");
  expect(screen.getByRole("option", { name: "claude-opus-4" })).toBeTruthy();
  expect(screen.getByRole("option", { name: "claude-sonnet-4" })).toBeTruthy();
  // Family group header, resolved from the hydrated catalog.
  expect(screen.getByText("Anthropic")).toBeTruthy();
});

test("lists app access rows from store data with their access switches", async () => {
  await renderView();

  expect(screen.getByText("App access")).toBeTruthy();
  expect(screen.getByText("GitHub")).toBeTruthy();
  const access = screen.getByRole("switch", { name: "GitHub access" });
  expect(access.getAttribute("aria-checked")).toBe("true");
});

test("shows the apps empty state when no apps are installed", async () => {
  useApps.setState({ apps: [], loaded: true });
  await renderView();

  expect(screen.getByText("No plugins installed yet — add MCP servers from the Plugins screen.")).toBeTruthy();
});

test("switching the permission mode calls updateRuntimeConfig and updates the description", async () => {
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Full" }));

  await waitFor(() => expect(updateRuntimeConfig).toHaveBeenCalledWith("claude", true, "sonnet", "full", ""));
  expect(await screen.findByText("Full access — no approval prompts.")).toBeTruthy();
  expect(screen.queryByText("Asks before edits and shell commands.")).toBeNull();
});

test("native default-model picker groups models with the Router default sentinel first", async () => {
  useRuntimes.setState({ runtimes: [claudeRuntime, nativeRuntime], loaded: true, refreshing: false, updating: {}, updateLog: {} });
  await renderView("native");

  fireEvent.click(screen.getByRole("combobox", { name: "Default model" }));
  const options = await screen.findAllByRole("option");
  expect(options[0]?.textContent).toBe("Router default (first usable provider)");
  // Bare route alias lands in the Route group, pinned first among groups.
  expect(screen.getByRole("option", { name: "smart" })).toBeTruthy();
  expect(screen.getByText("Route")).toBeTruthy();
  // Family-prefixed id renders trimmed under its family group.
  expect(screen.getByRole("option", { name: "claude-opus-4" })).toBeTruthy();
  expect(screen.getByText("Anthropic")).toBeTruthy();
});

test("tier model picker always shows the search input", async () => {
  await renderView();

  fireEvent.click(screen.getByRole("combobox", { name: "Smart model" }));
  expect(await screen.findByPlaceholderText("Search…")).toBeTruthy();
});

test("reset config uses the shared confirmation modal and restores focus", async () => {
  runtimeConfigStatus.mockImplementationOnce((_id: string) => ok({ ...configStatus, configured: true }));
  await renderView();
  const trigger = await screen.findByRole("button", { name: "Reset" });
  fireEvent.click(trigger);
  const dialog = screen.getByRole("dialog", { name: "Reset runtime config?" });
  expect(dialog.querySelector('[data-slot="modal-footer"]')).toBeTruthy();
  expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  await waitFor(() => expect(document.activeElement).toBe(trigger));
  expect(resetRuntimeConfig).not.toHaveBeenCalled();

  fireEvent.click(trigger);
  fireEvent.click(screen.getByRole("button", { name: "Reset" }));
  await waitFor(() => expect(resetRuntimeConfig).toHaveBeenCalledWith("claude"));
});
