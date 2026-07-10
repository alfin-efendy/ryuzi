import { describe, expect, test } from "bun:test";
import type { Project, RuntimeInfo } from "../bindings";
import { headerAgentLine } from "./session-header";

function runtime(model: string): RuntimeInfo {
  return {
    id: "native",
    name: "Ryuzi",
    color: "#888",
    initial: "R",
    connection: "Built-in",
    binaryPath: null,
    installedVersion: null,
    latestVersion: null,
    npmPackage: null,
    models: [],
    selectableModels: [],
    enabled: true,
    model,
    permMode: "default",
    flags: "",
    tiers: [],
    isDefault: true,
    runnable: true,
  };
}

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
  test("shows the project's pinned model, not the runtime card default", () => {
    expect(headerAgentLine(runtime("card-default"), project("anthropic/model-b"))).toBe("Ryuzi · anthropic/model-b");
  });

  test("falls back to the runtime card default when the project has no pin", () => {
    expect(headerAgentLine(runtime("card-default"), project(null))).toBe("Ryuzi · card-default");
  });

  test("falls back to the connection label when neither is set", () => {
    expect(headerAgentLine(runtime(""), project(null))).toBe("Ryuzi · Built-in");
  });

  test("no runtime card detected", () => {
    expect(headerAgentLine(undefined, project("m"))).toBe("No agent detected");
  });

  test("no project row (session list still loading)", () => {
    expect(headerAgentLine(runtime("card-default"), undefined)).toBe("Ryuzi · card-default");
  });
});
