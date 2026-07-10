import { describe, expect, test } from "bun:test";
import type { Project } from "../bindings";
import { headerAgentLine } from "./session-header";

function project(model: string | null): Project {
  return {
    projectId: "p1",
    name: "p1",
    workdir: "/w",
    source: null,
    harness: "native",
    model,
    effort: null,
    permMode: "default",
    createdAt: null,
    isGit: true,
  };
}

describe("headerAgentLine", () => {
  test("shows the project's pinned model, not the agent default", () => {
    expect(headerAgentLine(project("anthropic/model-b"), "agent-default")).toBe("Ryuzi · anthropic/model-b");
  });

  test("falls back to the agent's default model when the project has no pin", () => {
    expect(headerAgentLine(project(null), "agent-default")).toBe("Ryuzi · agent-default");
  });

  test("falls back to the router-default label when neither is set", () => {
    expect(headerAgentLine(project(null), null)).toBe("Ryuzi · Router default");
  });

  test("no project row (session list still loading)", () => {
    expect(headerAgentLine(undefined, "agent-default")).toBe("Ryuzi · agent-default");
  });
});
