import { expect, mock, test } from "bun:test";
import { fireEvent, render, screen } from "@testing-library/react";
import type { SelectableModelInfo } from "@/bindings";
import { ModelEffortDefaultCombobox, modelEffortDefaultOptions } from "./ProviderDetailView";

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

test("rendered_concrete_model_default_is_compact_and_clears_structured_key", async () => {
  const onChange = mock(() => undefined);
  const metadata: SelectableModelInfo = {
    kind: "concrete",
    requestValue: "openai/gpt-5",
    displayName: "GPT-5",
    preferenceKey: { family: "openai", model: "gpt-5" },
    supported: [
      { value: "low", label: "Low", description: null },
      { value: "high", label: "High", description: null },
    ],
    configuredDefault: "high",
    resolvedDefault: "high",
    defaultSource: "variesByTarget",
  };
  render(<ModelEffortDefaultCombobox metadata={metadata} onChange={onChange} />);
  const trigger = screen.getByRole("combobox", { name: "Default effort for GPT-5" });
  expect(trigger.textContent).toBe("Default: High");
  fireEvent.click(trigger);
  fireEvent.click(await screen.findByRole("option", { name: "Default: varies by target" }));
  expect(onChange).toHaveBeenCalledWith({ family: "openai", model: "gpt-5" }, null);
});
