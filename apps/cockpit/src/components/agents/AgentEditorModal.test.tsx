import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";
import type { AgentModelInfo, AgentRegistryInfo } from "@/bindings";

const route = (value: string): AgentModelInfo => ({ kind: "route", route: value });

mock.module("@/bindings", () => ({ commands: {}, events: {} }));

const { AgentEditorModal } = await import("./AgentEditorModal");
const { useAgents } = await import("@/store-agents");

const registry: AgentRegistryInfo = {
  agents: [],
  defaultAgentId: "ryuzi",
  recovery: [],
  subagentModel: route("fast"),
};

beforeEach(() => {
  useAgents.setState({
    registry,
    detail: null,
    models: [],
    loaded: true,
    loading: false,
    saving: false,
  });
});

afterEach(cleanup);

test("associates accessible names with every create field", () => {
  render(<AgentEditorModal open onClose={() => {}} />);

  expect(screen.getByRole("textbox", { name: "Name" })).toBeTruthy();
  expect(screen.getByRole("textbox", { name: "Description" })).toBeTruthy();
  expect(screen.getByRole("combobox", { name: "Avatar color" })).toBeTruthy();
});
