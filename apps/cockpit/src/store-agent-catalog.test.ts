import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import type { AgentConfigurationCatalogInfo, CmdError, Result } from "./bindings";

const catalog: AgentConfigurationCatalogInfo = {
  skills: [],
  nativeTools: [],
  pluginTools: [],
  apps: [],
};
const getAgentConfigurationCatalog = mock(
  async (): Promise<Result<AgentConfigurationCatalogInfo, CmdError>> => ({ status: "ok", data: catalog }),
);
mock.module("./bindings", () => ({ commands: { getAgentConfigurationCatalog } }));

const { useAgentConfigurationCatalog } = await import("./store-agent-catalog");

afterEach(() => {
  getAgentConfigurationCatalog.mockReset();
  getAgentConfigurationCatalog.mockResolvedValue({ status: "ok", data: catalog });
});
beforeEach(() => {
  useAgentConfigurationCatalog.setState({ catalog: null, loading: false, error: null });
});

test("loads the agent catalog once for concurrent consumers and exposes its state", async () => {
  const first = useAgentConfigurationCatalog.getState().load();
  const second = useAgentConfigurationCatalog.getState().load();

  await Promise.all([first, second]);

  expect(getAgentConfigurationCatalog).toHaveBeenCalledTimes(1);
  expect(getAgentConfigurationCatalog).toHaveBeenCalledWith("local");
  expect(useAgentConfigurationCatalog.getState()).toMatchObject({ catalog, loading: false, error: null });
});

test("exposes catalog request failures while leaving the catalog unset", async () => {
  getAgentConfigurationCatalog.mockResolvedValueOnce({ status: "error", error: { message: "catalog unavailable" } });

  await useAgentConfigurationCatalog.getState().load();

  expect(useAgentConfigurationCatalog.getState()).toMatchObject({ catalog: null, loading: false, error: "catalog unavailable" });
});
