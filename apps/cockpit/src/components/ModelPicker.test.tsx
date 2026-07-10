import { afterEach, beforeEach, expect, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { CatalogEntry, ConnectionInfo } from "@/bindings";

const catalog: CatalogEntry[] = [
  {
    id: "anthropic",
    name: "Anthropic",
    family: "anthropic",
    color: "#D97757",
    initial: "A",
    category: "api_key",
    format: "anthropic",
    requiresBaseUrl: false,
    models: ["claude-opus-4", "claude-sonnet-4"],
    freeTier: false,
    riskNotice: false,
    usesDeviceGrant: false,
  },
];

const connection: ConnectionInfo = {
  id: "conn-1",
  provider: "anthropic",
  providerName: "Anthropic",
  color: "#D97757",
  initial: "A",
  authType: "apiKey",
  label: "Anthropic",
  priority: 0,
  enabled: true,
  baseUrl: null,
  models: ["claude-opus-4", "claude-sonnet-4"],
  keyMasked: "sk-…3fk9",
  needsRelogin: false,
  claudeCloaking: false,
};

const { ModelPicker } = await import("./ModelPicker");
const { useConnections } = await import("@/store-connections");
const { useModelStatuses, statusKey } = await import("@/store-model-statuses");
const { useUi } = await import("@/store-ui");

const models = ["anthropic/claude-opus-4", "anthropic/claude-sonnet-4"];

beforeEach(() => {
  useConnections.setState({ catalog, connections: [connection], loaded: true });
  useModelStatuses.setState({ byKey: {} });
  useUi.setState({ hideInvalidModels: false });
});

// Reset the shared zustand singletons so later test files in the same bun
// process don't inherit this file's fixtures.
afterEach(() => {
  cleanup();
  useConnections.setState({ catalog: [], connections: [], loaded: false });
  useModelStatuses.setState({ byKey: {} });
  useUi.setState({ hideInvalidModels: false });
});

async function openPicker(name: string) {
  fireEvent.click(screen.getByRole("combobox", { name }));
  await screen.findByRole("listbox");
}

test("search input renders even with only two options", async () => {
  render(<ModelPicker value="" onValueChange={() => {}} models={models} variant="field" ariaLabel="Model" />);
  await openPicker("Model");
  expect(screen.getByPlaceholderText("Search…")).toBeTruthy();
  expect(screen.getAllByRole("option").length).toBe(2);
});

test("hide-invalid drops models with a persisted invalid verdict, keeps untested ones", async () => {
  useModelStatuses.setState({ byKey: { [statusKey("anthropic", "claude-sonnet-4")]: "invalid" } });
  useUi.setState({ hideInvalidModels: true });
  render(<ModelPicker value="" onValueChange={() => {}} models={models} variant="field" ariaLabel="Model" />);
  await openPicker("Model");
  expect(screen.getByRole("option", { name: "claude-opus-4" })).toBeTruthy();
  expect(screen.queryByRole("option", { name: "claude-sonnet-4" })).toBeNull();
});

test("the selected invalid model stays visible, flagged with the warning tone", async () => {
  useModelStatuses.setState({ byKey: { [statusKey("anthropic", "claude-sonnet-4")]: "invalid" } });
  useUi.setState({ hideInvalidModels: true });
  render(<ModelPicker value="anthropic/claude-sonnet-4" onValueChange={() => {}} models={models} variant="field" ariaLabel="Model" />);
  await openPicker("Model");
  // The sr-only "(invalid)" suffix is part of the accessible name.
  const kept = screen.getByRole("option", { name: /^claude-sonnet-4 \(invalid\)$/ });
  expect(kept.querySelector(".text-amber-500")).not.toBeNull();
});

test("leading sentinel options render first (field variant)", async () => {
  render(
    <ModelPicker
      value=""
      onValueChange={() => {}}
      models={models}
      leading={[{ value: "", label: "Router default (first usable provider)" }]}
      variant="field"
      ariaLabel="Default model"
    />,
  );
  await openPicker("Default model");
  const options = screen.getAllByRole("option");
  expect(options[0]?.textContent).toBe("Router default (first usable provider)");
  expect(options.length).toBe(3);
});

test("chip variant renders a trigger with the raw selected id and no default-trigger value slot", () => {
  render(<ModelPicker value="anthropic/claude-opus-4" onValueChange={() => {}} models={models} variant="chip" ariaLabel="Model" />);
  const chip = screen.getByRole("combobox", { name: "Model" });
  expect(chip.textContent).toContain("anthropic/claude-opus-4");
  expect(chip.querySelector('[data-slot="combobox-value"]')).toBeNull();
});

test("chip variant shows the placeholder when nothing is selected", () => {
  render(<ModelPicker value="" onValueChange={() => {}} models={models} variant="chip" placeholder="Default model" ariaLabel="Model" />);
  expect(screen.getByRole("combobox", { name: "Model" }).textContent).toContain("Default model");
});
