import { expect, test } from "bun:test";
import type { AgentRegistryInfo, AgentSummaryInfo } from "@/bindings";
import { defaultAgentModel } from "./default-agent-model";

function registry(model: AgentSummaryInfo["model"]): AgentRegistryInfo {
  return {
    agents: [
      {
        id: "ryuzi",
        name: "Ryuzi",
        description: "",
        avatarColor: "#000000",
        model,
        permissionMode: "ask",
        skillCount: 0,
        toolCount: 0,
        knowledgeCount: 0,
        executable: true,
        validation: [],
        isDefault: true,
      },
    ],
    defaultAgentId: "ryuzi",
    recovery: [],
    subagentModel: { kind: "route", route: "free" },
  };
}

test("default agent model fallback returns a route name", () => {
  expect(defaultAgentModel(registry({ kind: "route", route: "free" }))).toBe("free");
});

test("default agent model fallback returns a concrete model name", () => {
  expect(defaultAgentModel(registry({ kind: "concrete", name: "anthropic/claude-opus-4", effort: "high" }))).toBe(
    "anthropic/claude-opus-4",
  );
});

test("default agent model fallback tolerates a missing registry or default agent", () => {
  expect(defaultAgentModel(null)).toBeNull();
  expect(defaultAgentModel({ ...registry({ kind: "route", route: "free" }), defaultAgentId: "missing" })).toBeNull();
});
