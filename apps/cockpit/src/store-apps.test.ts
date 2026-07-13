import { expect, spyOn, test } from "bun:test";
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

test("concurrent hydration is deduplicated", async () => {
  useApps.setState({ apps: [], loaded: false, hydrating: false });
  let resolveList: ((value: { status: "ok"; data: AppInfo[] }) => void) | undefined;
  const pending = new Promise<{ status: "ok"; data: AppInfo[] }>((resolve) => {
    resolveList = resolve;
  });
  const spy = spyOn(commands, "listApps").mockReturnValue(pending);

  const first = useApps.getState().hydrate();
  const second = useApps.getState().hydrate();
  expect(spy).toHaveBeenCalledTimes(1);
  resolveList?.({ status: "ok", data: [makeApp()] });
  await Promise.all([first, second]);

  expect(useApps.getState().apps).toEqual([makeApp()]);
  expect(useApps.getState().hydrating).toBe(false);
  spy.mockRestore();
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
