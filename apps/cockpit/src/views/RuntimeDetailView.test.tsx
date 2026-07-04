import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AppInfo, ConnectionInfo, EndpointKeyInfo, EndpointStatusInfo, RuntimeConfigStatusInfo, RuntimeInfo } from "@/bindings";

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
  baseUrl: null,
  models: ["claude-sonnet-4"],
  keyMasked: "sk-…3fk9",
};

const endpointUp: EndpointStatusInfo = { running: true, port: 8787, baseUrl: "http://127.0.0.1:8787", autostart: false };
const endpointKey: EndpointKeyInfo = { id: "key-1", name: "local", key: "rz-abc123", createdAt: 1, lastUsedAt: null };
const configStatus: RuntimeConfigStatusInfo = {
  configPath: "/home/user/.claude/settings.json",
  exists: true,
  configured: false,
  supported: true,
};

const ok = <T,>(data: T) => Promise.resolve({ status: "ok" as const, data });

const runtimeConfigStatus = mock((_id: string) => ok(configStatus));
const updateRuntimeConfig = mock((_id: string, enabled: boolean, model: string | null, permMode: string, flags: string) =>
  ok([{ ...claudeRuntime, enabled, model: model ?? "", permMode, flags }]),
);

// Mock the Tauri IPC boundary before the component (and the stores it pulls
// in) resolve "@/bindings"; only mount-time commands need implementations.
mock.module("@/bindings", () => ({
  commands: {
    runtimeConfigStatus,
    updateRuntimeConfig,
    endpointStatus: () => ok(endpointUp),
    listEndpointKeys: () => ok([endpointKey]),
    listProviderCatalog: () => ok([]),
    listConnections: () => ok([anthropicConnection]),
    listApps: () => ok([githubApp]),
    listRuntimes: () => ok([claudeRuntime]),
    refreshRuntimes: () => ok([claudeRuntime]),
  },
}));

const { RuntimeDetailView } = await import("./RuntimeDetailView");
const { useRuntimes } = await import("@/store-runtimes");
const { useApps } = await import("@/store-apps");
const { useEndpoint } = await import("@/store-endpoint");
const { useConnections } = await import("@/store-connections");

beforeEach(() => {
  runtimeConfigStatus.mockClear();
  updateRuntimeConfig.mockClear();
  useRuntimes.setState({ runtimes: [claudeRuntime], loaded: true, refreshing: false, updating: {}, updateLog: {} });
  useApps.setState({ apps: [githubApp], loaded: true, probing: null });
  useEndpoint.setState({ status: null, keys: [], loaded: false });
  useConnections.setState({ catalog: [], connections: [], loaded: false });
});

afterEach(cleanup);

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

  // Claude gets Opus/Sonnet/Haiku tier pickers plus the default-model picker,
  // each offering the provider/model options from the enabled connection.
  expect(screen.getByText("Opus")).toBeTruthy();
  await waitFor(() => expect(screen.getAllByRole("option", { name: "anthropic/claude-sonnet-4" }).length).toBe(4));
  await waitFor(() => expect((screen.getByRole("button", { name: "Apply" }) as HTMLButtonElement).disabled).toBe(false));
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

  expect(screen.getByText("No apps installed yet — add MCP servers from the Apps screen.")).toBeTruthy();
});

test("switching the permission mode calls updateRuntimeConfig and updates the description", async () => {
  await renderView();

  fireEvent.click(screen.getByRole("button", { name: "Full" }));

  await waitFor(() => expect(updateRuntimeConfig).toHaveBeenCalledWith("claude", true, "sonnet", "full", ""));
  expect(await screen.findByText("Full access — no approval prompts.")).toBeTruthy();
  expect(screen.queryByText("Asks before edits and shell commands.")).toBeNull();
});
