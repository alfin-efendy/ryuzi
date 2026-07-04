import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { ConnectionInfo, EndpointKeyInfo, EndpointStatusInfo, UsageSeries } from "@/bindings";

const status: EndpointStatusInfo = { running: true, port: 8899, baseUrl: "http://127.0.0.1:8899/v1", autostart: false };

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
};

const usage: UsageSeries = {
  days: [
    { day: "2026-07-03", requests: 4, inputTokens: 1200, outputTokens: 300 },
    { day: "2026-07-04", requests: 3, inputTokens: 900, outputTokens: 210 },
  ],
  todayRequests: 3,
  todayInputTokens: 900,
  todayOutputTokens: 210,
};

// Mock the Tauri IPC boundary before the component (and the stores it uses) load.
mock.module("@/bindings", () => ({
  commands: {
    endpointStatus: () => Promise.resolve({ status: "ok", data: status }),
    listEndpointKeys: () => Promise.resolve({ status: "ok", data: keys }),
    listProviderCatalog: () => Promise.resolve({ status: "ok", data: [] }),
    listConnections: () => Promise.resolve({ status: "ok", data: [connection] }),
    endpointUsage: () => Promise.resolve({ status: "ok", data: usage }),
  },
}));

const { ModelsView } = await import("./ModelsView");
const { useEndpoint } = await import("@/store-endpoint");
const { useConnections } = await import("@/store-connections");
const { useUsage } = await import("@/store-usage");

afterEach(() => {
  cleanup();
  useEndpoint.setState({ status: null, keys: [], loaded: false });
  useConnections.setState({ catalog: [], connections: [], loaded: false });
  useUsage.setState({ byConnection: {}, endpoint: null });
});

test("renders the Models heading and endpoint status after hydration", async () => {
  render(<ModelsView />);

  await screen.findByText("Running on http://127.0.0.1:8899/v1");
  expect(screen.getByRole("heading", { level: 2, name: "Models" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Stop" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Endpoint" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Providers" })).toBeTruthy();
});

test("seeds the settings form from the hydrated endpoint status", async () => {
  render(<ModelsView />);

  expect(await screen.findByDisplayValue("8899")).toBeTruthy();
  const autostart = screen.getByRole("switch", { name: "Start automatically with Cockpit" });
  expect(autostart.getAttribute("aria-checked")).toBe("false");
  expect(screen.getByRole("button", { name: "Save" })).toBeTruthy();
});

test("lists endpoint API keys with revoke and create controls", async () => {
  render(<ModelsView />);

  await screen.findByText("VS Code");
  expect(screen.getByText("rz-live-abc123")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Revoke" })).toBeTruthy();
  const newKey = screen.getByRole("button", { name: "New key" }) as HTMLButtonElement;
  expect(newKey.disabled).toBe(true);
});

test("switching to the Providers tab shows connections instead of endpoint settings", async () => {
  render(<ModelsView />);
  await screen.findByText("Running on http://127.0.0.1:8899/v1");

  fireEvent.click(screen.getByRole("button", { name: "Providers" }));

  await screen.findByText("Work OpenAI");
  expect(screen.getByRole("switch", { name: "Enabled" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Test" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Add connection" })).toBeTruthy();
  expect(screen.queryByText("Running on http://127.0.0.1:8899/v1")).toBeNull();
});

test("shows empty states for API keys and connections", async () => {
  useEndpoint.setState({ status, keys: [], loaded: true });
  useConnections.setState({ catalog: [], connections: [], loaded: true });
  render(<ModelsView />);

  expect(await screen.findByText("No API keys yet — create one for external tools.")).toBeTruthy();
  fireEvent.click(screen.getByRole("button", { name: "Providers" }));
  expect(screen.getByText("No connections yet. Add a provider connection to route models through Ryuzi.")).toBeTruthy();
});
