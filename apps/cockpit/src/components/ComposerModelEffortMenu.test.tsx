import { expect, mock, test } from "bun:test";
import { fireEvent, render, screen } from "@testing-library/react";
import { ComposerModelEffortMenu } from "./ComposerModelEffortMenu";

const models = [
  {
    kind: "concrete" as const,
    requestValue: "openai/gpt-5",
    displayName: "GPT-5",
    preferenceKey: { family: "openai", model: "gpt-5" },
    supported: [
      { value: "low", label: "Low", description: "Fast responses" },
      { value: "high", label: "High", description: "Deeper reasoning" },
      { value: "custom", label: "Custom", description: null },
    ],
    configuredDefault: null,
    resolvedDefault: "high",
    defaultSource: "provider" as const,
  },
];
const anthropicModel = {
  ...models[0],
  requestValue: "anthropic/claude-opus-4",
  displayName: "Claude Opus",
  preferenceKey: { family: "anthropic", model: "claude-opus-4" },
};

test("renders_model_and_dynamic_effort_rows_without_advanced_or_speed", () => {
  render(
    <ComposerModelEffortMenu
      models={[anthropicModel, ...models]}
      runtime={{
        projectId: "p1",
        model: "openai/gpt-5",
        storedEffort: "custom",
        effectiveEffort: "custom",
        effectiveEffortLabel: "Custom",
        effectiveSource: "project",
        storedEffortStatus: "valid",
        modelInfo: models[0],
      }}
      onChange={() => undefined}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /model and effort/i }));
  expect(screen.getAllByText("GPT-5").length).toBeGreaterThan(0);
  expect(screen.getAllByText("Custom").length).toBeGreaterThan(0);
  expect(screen.getByText("Fast responses")).toBeTruthy();
  expect(screen.getByText("Deeper reasoning")).toBeTruthy();
  const claude = screen.getByText("Claude Opus");
  const gptMatches = screen.getAllByText("GPT-5");
  const gpt = gptMatches[gptMatches.length - 1];
  expect(claude.compareDocumentPosition(gpt) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
  const customMatches = screen.getAllByText("Custom");
  expect(customMatches[customMatches.length - 1]?.closest("button")?.querySelector("svg")).toBeTruthy();
  expect(screen.queryByText("Advanced")).toBeNull();
  expect(screen.queryByText("Speed")).toBeNull();
});

test("hides_zero_option_effort_and_marks_one_option_read_only", () => {
  const first = render(<ComposerModelEffortMenu models={[{ ...models[0], supported: [] }]} runtime={null} onChange={() => undefined} />);
  fireEvent.click(screen.getByRole("button", { name: /model and effort/i }));
  expect(screen.queryByText("Effort")).toBeNull();
  first.unmount();
  const onChange = mock(() => undefined);
  render(
    <ComposerModelEffortMenu
      models={[{ ...models[0], supported: [models[0].supported[0]] }]}
      runtime={{
        projectId: "p1",
        model: "openai/gpt-5",
        storedEffort: null,
        effectiveEffort: "low",
        effectiveEffortLabel: "Low",
        effectiveSource: "provider",
        storedEffortStatus: "valid",
        modelInfo: { ...models[0], supported: [models[0].supported[0]] },
      }}
      onChange={onChange}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /model and effort/i }));
  expect(screen.getByText(/read.only/i)).toBeTruthy();
  const readOnly = screen.getByText(/read.only/i).closest("fieldset:disabled[data-readonly]");
  expect(readOnly).toBeTruthy();
  fireEvent.click(screen.getByText("Low"));
  fireEvent.keyDown(screen.getByText("Low"), { key: "Enter" });
  expect(onChange).not.toHaveBeenCalled();
});

test("one_option_stale_effort_shows_read_only_default_and_warning", () => {
  const only = { ...models[0], supported: [models[0].supported[0]], resolvedDefault: "low" };
  render(
    <ComposerModelEffortMenu
      models={[only]}
      runtime={{
        projectId: "p1",
        model: only.requestValue,
        storedEffort: "extreme",
        effectiveEffort: "low",
        effectiveEffortLabel: "Low",
        effectiveSource: "provider",
        storedEffortStatus: "unsupported",
        modelInfo: only,
      }}
      onChange={() => undefined}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /model and effort/i }));
  expect(screen.getAllByText(/model default.*low/i).length).toBeGreaterThan(0);
  expect(screen.getByText(/extreme.*unsupported/i)).toBeTruthy();
  const defaultLabels = screen.getAllByText(/model default.*low/i);
  expect(defaultLabels[defaultLabels.length - 1].closest("fieldset:disabled[data-readonly]")).toBeTruthy();
});

test("one_option_unknown_metadata_never_claims_unsupported", () => {
  const only = { ...models[0], supported: [models[0].supported[0]], resolvedDefault: "low" };
  render(
    <ComposerModelEffortMenu
      models={[only]}
      runtime={{
        projectId: "p1",
        model: only.requestValue,
        storedEffort: "future",
        effectiveEffort: null,
        effectiveEffortLabel: null,
        effectiveSource: "none",
        storedEffortStatus: "unknownMetadata",
        modelInfo: only,
      }}
      onChange={() => undefined}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /model and effort/i }));
  expect(screen.getByText(/metadata unknown/i)).toBeTruthy();
  expect(screen.queryByText(/unsupported/i)).toBeNull();
});

test("stale_effort_uses_effective_default_and_shows_compact_warning", () => {
  render(
    <ComposerModelEffortMenu
      models={models}
      runtime={{
        projectId: "p1",
        model: "openai/gpt-5",
        storedEffort: "extreme",
        effectiveEffort: "high",
        effectiveEffortLabel: "High",
        effectiveSource: "provider",
        storedEffortStatus: "unsupported",
        modelInfo: models[0],
      }}
      onChange={() => undefined}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /model and effort/i }));
  expect(screen.getByText(/extreme.*unsupported/i)).toBeTruthy();
  expect(screen.getAllByText(/model default.*high/i).length).toBeGreaterThan(0);
});

test("unknown_metadata_is_not_reported_as_unsupported_or_cleared", () => {
  render(
    <ComposerModelEffortMenu
      models={models}
      runtime={{
        projectId: "p1",
        model: "unknown/model",
        storedEffort: "future",
        effectiveEffort: null,
        effectiveEffortLabel: null,
        effectiveSource: "none",
        storedEffortStatus: "unknownMetadata",
        modelInfo: null,
      }}
      onChange={() => undefined}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /model and effort/i }));
  expect(screen.getByText(/metadata unknown/i)).toBeTruthy();
  expect(screen.queryByText(/unsupported/i)).toBeNull();
});

test("model_default_clears_project_effort", () => {
  const onChange = mock(() => undefined);
  render(
    <ComposerModelEffortMenu
      models={models}
      runtime={{
        projectId: "p1",
        model: models[0].requestValue,
        storedEffort: "high",
        effectiveEffort: "high",
        effectiveEffortLabel: "High",
        effectiveSource: "project",
        storedEffortStatus: "valid",
        modelInfo: models[0],
      }}
      onChange={onChange}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /model and effort/i }));
  fireEvent.click(screen.getByText(/model default.*high/i));
  expect(onChange).toHaveBeenCalledWith("openai/gpt-5", null);
});

test("model_change_preserves_only_supported_project_effort", () => {
  const onChange = mock(() => undefined);
  const second = { ...models[0], requestValue: "openai/gpt-5-mini", displayName: "GPT-5 mini", supported: [models[0].supported[1]] };
  render(
    <ComposerModelEffortMenu
      models={[...models, second]}
      runtime={{
        projectId: "p1",
        model: models[0].requestValue,
        storedEffort: "high",
        effectiveEffort: "high",
        effectiveEffortLabel: "High",
        effectiveSource: "project",
        storedEffortStatus: "valid",
        modelInfo: models[0],
      }}
      onChange={onChange}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /model and effort/i }));
  fireEvent.click(screen.getByText("GPT-5 mini"));
  expect(onChange).toHaveBeenCalledWith("openai/gpt-5-mini", "high");
});

test("model_change_clears_unsupported_project_effort", () => {
  const onChange = mock(() => undefined);
  const second = { ...models[0], requestValue: "openai/gpt-5-mini", displayName: "GPT-5 mini", supported: [models[0].supported[0]] };
  render(
    <ComposerModelEffortMenu
      models={[...models, second]}
      runtime={{
        projectId: "p1",
        model: models[0].requestValue,
        storedEffort: "high",
        effectiveEffort: "high",
        effectiveEffortLabel: "High",
        effectiveSource: "project",
        storedEffortStatus: "valid",
        modelInfo: models[0],
      }}
      onChange={onChange}
    />,
  );
  fireEvent.click(screen.getByRole("button", { name: /model and effort/i }));
  fireEvent.click(screen.getByText("GPT-5 mini"));
  expect(onChange).toHaveBeenCalledWith("openai/gpt-5-mini", null);
});

test("running_change_announces_project_next_turns", () => {
  render(<ComposerModelEffortMenu models={models} runtime={null} onChange={() => undefined} running />);
  fireEvent.click(screen.getByRole("button", { name: /model and effort/i }));
  expect(screen.getByText(/project.*next turns/i)).toBeTruthy();
});
