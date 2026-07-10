import { expect, test } from "bun:test";
import type { SelectableModelInfo } from "@/bindings";
import { modelEffortDefaultOptions } from "./ProviderDetailView";

test("provider_model_default_selector_clears_and_reports_varied_defaults", () => {
  const metadata: SelectableModelInfo = {
    kind: "namedRoute",
    requestValue: "smart",
    displayName: "Smart",
    preferenceKey: { family: "openai", model: "gpt-5" },
    supported: [{ value: "high", label: "High", description: "Deep reasoning" }],
    configuredDefault: null,
    resolvedDefault: null,
    defaultSource: "variesByTarget",
  };
  expect(modelEffortDefaultOptions(metadata)).toEqual([
    { value: "__model_default__", label: "Default: varies by target" },
    { value: "high", label: "High", description: "Deep reasoning" },
  ]);
});
