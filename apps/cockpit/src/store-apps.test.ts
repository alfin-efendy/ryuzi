import { test, expect, spyOn } from "bun:test";
import { agentAllowed, useApps } from "./store-apps";
import { commands, type AppInfo } from "./bindings";
import { LOCAL_RUNNER } from "@/lib/session-key";

function makeApp(overrides: Partial<AppInfo> = {}): AppInfo {
  return {
    id: "github",
    name: "GitHub",
    kind: "MCP · stdio",
    initial: "G",
    color: "#8B5CF6",
    desc: "",
    transport: "stdio",
    command: "gh-mcp",
    args: [],
    url: null,
    scope: "global",
    scopeGateways: [],
    status: "connected",
    statusDetail: null,
    version: null,
    publisher: null,
    authKind: "none",
    authDetail: null,
    tools: [],
    agentAccess: [{ agentId: "native", allowed: true }],
    ...overrides,
  };
}

test("agentAllowed reads the native row and defaults to allowed", () => {
  expect(agentAllowed(makeApp())).toBe(true);
  expect(agentAllowed(makeApp({ agentAccess: [{ agentId: "native", allowed: false }] }))).toBe(false);
  expect(agentAllowed(makeApp({ agentAccess: [] }))).toBe(true);
});

test("toggleAgent flips the native row optimistically and persists agent_id native", async () => {
  useApps.setState({ apps: [makeApp()], loaded: true, probing: null });
  const spy = spyOn(commands, "toggleAppAgent").mockResolvedValue({
    status: "ok",
    data: [makeApp({ agentAccess: [{ agentId: "native", allowed: false }] })],
  });
  await useApps.getState().toggleAgent("github", false);
  expect(spy).toHaveBeenCalledWith(LOCAL_RUNNER, "github", "native", false);
  expect(agentAllowed(useApps.getState().apps[0])).toBe(false);
  spy.mockRestore();
  useApps.setState({ apps: [], loaded: false });
});
