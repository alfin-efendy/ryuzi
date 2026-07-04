import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { GatewayEventInfo, GatewayInfo, Session } from "@/bindings";

// The view renders straight from the gateways/session stores and only touches
// IPC through store actions, so we mock the Tauri bindings boundary and seed
// zustand state directly.

const localGateway: GatewayInfo = {
  id: "local",
  name: "Local Machine",
  badge: "LM",
  kind: "local",
  detail: "This computer — always available",
  metaLine: "Local · direct",
  status: "connected",
  latency: "0.4 ms",
  daemonVersion: "0.4.0",
  uptime: "3d 2h",
  lastSeenMs: Date.now(),
  resources: [{ label: "CPU", sub: "8 cores", pct: 42 }],
  fingerprint: null,
  fsMode: "projects",
  paths: ["/home/dev/projects"],
};

const daemonEvents: GatewayEventInfo[] = [{ at: 1_720_000_000_000, level: "info", text: "Daemon handshake complete" }];

const runningSession: Session = {
  sessionPk: "s-run",
  projectId: "p-1",
  agentSessionId: null,
  worktreePath: null,
  branch: "main",
  title: "Fix flaky tests",
  status: "running",
  startedBy: null,
  createdAt: 1_720_000_000_000,
  lastActive: 1_720_000_000_000,
  resumeAttempts: 0,
};
const endedSession: Session = { ...runningSession, sessionPk: "s-done", title: "Old finished run", status: "ended" };

const gatewayEvents = mock(async (_id: string) => ({ status: "ok" as const, data: daemonEvents }));
const updateGateway = mock(async (id: string, fsMode: string, paths: string[]) => ({
  status: "ok" as const,
  data: [{ ...localGateway, id, fsMode, paths, latency: "0.9 ms" }],
}));
const probeGateways = mock(async () => ({ status: "ok" as const, data: [localGateway] }));
const removeGateway = mock(async (_id: string) => ({ status: "ok" as const, data: [] as GatewayInfo[] }));
const pickDirectory = mock(async () => null);

mock.module("@/bindings", () => ({
  commands: { gatewayEvents, updateGateway, probeGateways, removeGateway, pickDirectory },
  events: {},
}));

const { GatewayDetailView } = await import("@/views/GatewayDetailView");
const { useGateways } = await import("@/store-gateways");
const { useStore } = await import("@/store");

function seed() {
  useGateways.setState({ gateways: [localGateway], eventsById: {}, loaded: true, probing: false });
  useStore.setState({ sessions: [runningSession, endedSession], focusedSessionPk: null });
}

afterEach(() => {
  cleanup();
  gatewayEvents.mockClear();
  updateGateway.mockClear();
});

test("renders identity, status, and health for a connected gateway", async () => {
  seed();
  render(<GatewayDetailView id="local" />);
  await screen.findByText(/Daemon handshake complete/);

  expect(screen.getByText("Local Machine")).toBeTruthy();
  expect(screen.getByText("Connected")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Probe now" }).hasAttribute("disabled")).toBe(false);
  expect(screen.getByText("0.4 ms")).toBeTruthy();
  expect(screen.getByText("CPU")).toBeTruthy();
  expect(screen.getByText("42%")).toBeTruthy();
});

test("lists live sessions on the local gateway and hides ended ones", async () => {
  seed();
  render(<GatewayDetailView id="local" />);
  await screen.findByText(/Daemon handshake complete/);

  expect(screen.getByText("Sessions · 1")).toBeTruthy();
  expect(screen.getByRole("button", { name: /Fix flaky tests/ })).toBeTruthy();
  expect(screen.queryByText("Old finished run")).toBeNull();
});

test("loads daemon events on mount and renders the log card", async () => {
  seed();
  render(<GatewayDetailView id="local" />);

  expect(await screen.findByText(/Daemon handshake complete/)).toBeTruthy();
  expect(gatewayEvents).toHaveBeenCalledWith("local");
  expect(screen.getByText("Daemon events, most recent last")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Copy" })).toBeTruthy();
});

test("switching filesystem access persists the mode and re-renders from the IPC result", async () => {
  seed();
  render(<GatewayDetailView id="local" />);
  await screen.findByText(/Daemon handshake complete/);
  expect(screen.getByRole("button", { name: "/home/dev/projects" })).toBeTruthy();

  fireEvent.click(screen.getByRole("button", { name: "Read-only" }));

  expect(updateGateway).toHaveBeenCalledWith("local", "read", ["/home/dev/projects"]);
  // Health re-renders from the mocked updateGateway response.
  expect(await screen.findByText("0.9 ms")).toBeTruthy();
  expect(screen.getByText("Agents can inspect files but never write outside a worktree.")).toBeTruthy();
  expect(screen.queryByRole("button", { name: "/home/dev/projects" })).toBeNull();
});

test("shows the not-found state for an unknown gateway id", async () => {
  seed();
  render(<GatewayDetailView id="ghost" />);

  expect(screen.getByText("Gateway not found.")).toBeTruthy();
  // The mount effect still fetches events; wait for it so the update lands inside the test.
  await waitFor(() => expect(useGateways.getState().eventsById.ghost).toBeDefined());
});
