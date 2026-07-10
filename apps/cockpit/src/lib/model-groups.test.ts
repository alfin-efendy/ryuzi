import { expect, test } from "bun:test";
import { groupModelOptions, modelStatusKey, withLeadingOption } from "./model-groups";
import type { CatalogEntry, ConnectionInfo } from "../bindings";
import { statusKey } from "../store-model-statuses";

const entry = (id: string, family: string, name: string, models: string[] = []): CatalogEntry =>
  ({
    id,
    name,
    family,
    color: "#000",
    initial: name[0],
    category: "llm",
    format: "anthropic",
    requiresBaseUrl: false,
    models,
  }) as CatalogEntry;
const conn = (provider: string, models: string[], enabled = true): ConnectionInfo =>
  ({
    id: `c-${provider}`,
    provider,
    providerName: provider,
    color: "#000",
    initial: "x",
    authType: "apiKey",
    label: provider,
    priority: 0,
    enabled,
    baseUrl: null,
    models,
    keyMasked: null,
    needsRelogin: false,
    claudeCloaking: false,
  }) as ConnectionInfo;

const catalog = [
  entry("anthropic", "anthropic", "Anthropic", ["claude-fable-5"]),
  entry("anthropic-oauth", "anthropic", "Anthropic (OAuth)", []),
  entry("openai", "openai", "OpenAI", []),
  entry("openrouter", "openrouter", "OpenRouter", []),
];

test("groups runtime models by connected provider family; unmatched bare ids are routes, first", () => {
  const groups = groupModelOptions(["claude-fable-5", "claude-opus-4-8", "gpt-5.5", "mystery-model"], catalog, [
    conn("anthropic-oauth", ["claude-opus-4-8"]),
    conn("openai", ["gpt-5.5"]),
  ]);
  expect(groups).toEqual([
    { label: "Route", options: [{ value: "mystery-model", label: "mystery-model", mono: true }] },
    {
      label: "Anthropic",
      options: [
        { value: "claude-fable-5", label: "claude-fable-5", mono: true },
        { value: "claude-opus-4-8", label: "claude-opus-4-8", mono: true },
      ],
    },
    { label: "OpenAI", options: [{ value: "gpt-5.5", label: "gpt-5.5", mono: true }] },
  ]);
});

test("disabled connections don't contribute; no catalog → flat list", () => {
  const flat = groupModelOptions(["m1"], [], []);
  expect(flat).toEqual([{ value: "m1", label: "m1", mono: true }]);
  const noGroups = groupModelOptions(["gpt-5.5"], catalog, [conn("openai", ["gpt-5.5"], false)]);
  expect(noGroups).toEqual([{ value: "gpt-5.5", label: "gpt-5.5", mono: true }]);
});

test("family-prefixed runtime ids group by prefix with trimmed labels; bare route id pinned first", () => {
  const groups = groupModelOptions(["anthropic/claude-fable-5", "openai/gpt-5.2", "low_task"], catalog, [
    conn("anthropic-oauth", ["claude-opus-4-8"]),
    conn("openai", ["gpt-5.5"]),
  ]);
  expect(groups).toEqual([
    { label: "Route", options: [{ value: "low_task", label: "low_task", mono: true }] },
    {
      label: "Anthropic",
      options: [{ value: "anthropic/claude-fable-5", label: "claude-fable-5", mono: true }],
    },
    { label: "OpenAI", options: [{ value: "openai/gpt-5.2", label: "gpt-5.2", mono: true }] },
  ]);
});

test("unknown prefix lands in Other with the full id as label", () => {
  const groups = groupModelOptions(["anthropic/claude-fable-5", "mystery/whatever"], catalog, [
    conn("anthropic-oauth", ["claude-opus-4-8"]),
  ]);
  expect(groups).toEqual([
    {
      label: "Anthropic",
      options: [{ value: "anthropic/claude-fable-5", label: "claude-fable-5", mono: true }],
    },
    { label: "Other", options: [{ value: "mystery/whatever", label: "mystery/whatever", mono: true }] },
  ]);
});

test("catalog-entry-id prefix resolves to its family with a trimmed label", () => {
  // RuntimeDetailView's endpoint card builds `${connection.provider}/${model}`
  // ids, where provider is a catalog ENTRY id (anthropic-oauth), not a family.
  const groups = groupModelOptions(["anthropic-oauth/claude-opus-4-8"], catalog, []);
  expect(groups).toEqual([
    {
      label: "Anthropic",
      options: [{ value: "anthropic-oauth/claude-opus-4-8", label: "claude-opus-4-8", mono: true }],
    },
  ]);
});

test("withLeadingOption prepends a bare option to a flat list", () => {
  const sentinel = { value: "", label: "Router default (first usable provider)" };
  expect(withLeadingOption(sentinel, [{ value: "m1", label: "m1", mono: true }])).toEqual([
    sentinel,
    { value: "m1", label: "m1", mono: true },
  ]);
  expect(withLeadingOption(sentinel, [])).toEqual([sentinel]);
});

test("withLeadingOption wraps the sentinel in a headingless group ahead of a grouped list", () => {
  const sentinel = { value: "__combo__", label: "Route by task (combo)" };
  const groups = groupModelOptions(["anthropic/claude-fable-5"], catalog, []);
  expect(withLeadingOption(sentinel, groups)).toEqual([
    { label: "", options: [sentinel] },
    { label: "Anthropic", options: [{ value: "anthropic/claude-fable-5", label: "claude-fable-5", mono: true }] },
  ]);
});

test("modelStatusKey splits family::model route-target keys", () => {
  expect(modelStatusKey("openai::gpt-5.5", catalog)).toEqual({ family: "openai", model: "gpt-5.5" });
});

test("modelStatusKey resolves family/model runtime ids", () => {
  expect(modelStatusKey("anthropic/claude-fable-5", catalog)).toEqual({ family: "anthropic", model: "claude-fable-5" });
});

test("modelStatusKey resolves entry-id/model to the entry's family via the catalog", () => {
  expect(modelStatusKey("anthropic-oauth/claude-opus-4-8", catalog)).toEqual({
    family: "anthropic",
    model: "claude-opus-4-8",
  });
});

test("modelStatusKey returns null for bare route aliases and unknown prefixes", () => {
  expect(modelStatusKey("smart", catalog)).toBeNull();
  expect(modelStatusKey("low_task", catalog)).toBeNull();
  expect(modelStatusKey("mystery/whatever", catalog)).toBeNull();
});

test("model_groups_never_strip_effort_suffixes", () => {
  expect(modelStatusKey("openai/gpt-5.5-codex-low", catalog)).toEqual({ family: "openai", model: "gpt-5.5-codex-low" });
  expect(modelStatusKey("openai/gpt-5.5-codex-medium", catalog)).toEqual({ family: "openai", model: "gpt-5.5-codex-medium" });
  expect(modelStatusKey("openai/gpt-5.5-codex-high", catalog)).toEqual({ family: "openai", model: "gpt-5.5-codex-high" });
  // "-xhigh" must strip whole, not leave "…-x" behind by matching "-high".
  expect(modelStatusKey("openai/gpt-5.5-codex-xhigh", catalog)).toEqual({ family: "openai", model: "gpt-5.5-codex-xhigh" });
  expect(modelStatusKey("openai/gpt-5.6-luna-none", catalog)).toEqual({ family: "openai", model: "gpt-5.6-luna-none" });
  expect(modelStatusKey("openai/gpt-5.6-luna-review", catalog)).toEqual({ family: "openai", model: "gpt-5.6-luna-review" });
  // Review strips first, then ONE effort suffix — same one-pass order as Rust.
  expect(modelStatusKey("openai/gpt-5.5-codex-high-review", catalog)).toEqual({ family: "openai", model: "gpt-5.5-codex-high-review" });
  // Suffix stripping applies to route-target keys too.
  expect(modelStatusKey("openai::gpt-5.5-codex-review", catalog)).toEqual({ family: "openai", model: "gpt-5.5-codex-review" });
  // Non-variant models pass through untouched.
  expect(modelStatusKey("anthropic/claude-fable-5", catalog)).toEqual({ family: "anthropic", model: "claude-fable-5" });
});

test("modelStatusKey scopes codex suffix stripping to the openai family — non-openai ids keep their suffix", () => {
  // OpenRouter's `openai/o3-mini-high` is a real, distinct model id, not a
  // Codex effort-variant of `openai/o3-mini`. Only the `openai` family has
  // synthetic effort/-review picker variants (Rust's codex_base_model runs
  // only on the openai-oauth probe), so a genuine non-openai id ending in
  // "-high" must NOT be truncated.
  expect(modelStatusKey("openrouter/openai/o3-mini-high", catalog)).toEqual({
    family: "openrouter",
    model: "openai/o3-mini-high",
  });
});

test("hideInvalid drops options with a persisted invalid verdict; untested stay", () => {
  const statuses = { [statusKey("openai", "gpt-5.5")]: "invalid" as const };
  const groups = groupModelOptions(["anthropic/claude-fable-5", "openai/gpt-5.5"], catalog, [], {
    statuses,
    hideInvalid: true,
  });
  expect(groups).toEqual([{ label: "Anthropic", options: [{ value: "anthropic/claude-fable-5", label: "claude-fable-5", mono: true }] }]);
});

test("the selected invalid model stays visible, flagged invalid", () => {
  const statuses = { [statusKey("openai", "gpt-5.5")]: "invalid" as const };
  const groups = groupModelOptions(["anthropic/claude-fable-5", "openai/gpt-5.5"], catalog, [], {
    statuses,
    hideInvalid: true,
    selectedValue: "openai/gpt-5.5",
  });
  expect(groups).toEqual([
    { label: "Anthropic", options: [{ value: "anthropic/claude-fable-5", label: "claude-fable-5", mono: true }] },
    { label: "OpenAI", options: [{ value: "openai/gpt-5.5", label: "gpt-5.5", mono: true, invalid: true }] },
  ]);
});

test("invalid options are flagged (not hidden) when hideInvalid is off", () => {
  const statuses = { [statusKey("openai", "gpt-5.5")]: "invalid" as const };
  const groups = groupModelOptions(["openai/gpt-5.5"], catalog, [], { statuses, hideInvalid: false });
  expect(groups).toEqual([{ label: "OpenAI", options: [{ value: "openai/gpt-5.5", label: "gpt-5.5", mono: true, invalid: true }] }]);
});

test("route aliases are never filtered; grouping survives filtering", () => {
  const statuses = { [statusKey("openai", "gpt-5.5")]: "invalid" as const };
  const groups = groupModelOptions(["smart", "openai/gpt-5.5", "anthropic/claude-fable-5"], catalog, [], {
    statuses,
    hideInvalid: true,
  });
  expect(groups).toEqual([
    { label: "Route", options: [{ value: "smart", label: "smart", mono: true }] },
    { label: "Anthropic", options: [{ value: "anthropic/claude-fable-5", label: "claude-fable-5", mono: true }] },
  ]);
});

test("the ungrouped fallback honors hide-invalid too (no resurrection)", () => {
  // All grouped options filtered away → byFamily is empty → the fallback
  // must return the FILTERED flat list, not the raw input.
  const statuses = { [statusKey("openai", "gpt-5.5")]: "invalid" as const };
  const flat = groupModelOptions(["smart", "openai/gpt-5.5"], catalog, [], { statuses, hideInvalid: true });
  expect(flat).toEqual([{ value: "smart", label: "smart", mono: true }]);
});

test("hideInvalid resolves bare ids without applying a base verdict to effort suffixes", () => {
  const statuses = { [statusKey("openai", "gpt-5.5-codex")]: "invalid" as const };
  const groups = groupModelOptions(
    ["gpt-5.5-codex", "openai/gpt-5.5-codex-high", "claude-fable-5"],
    catalog,
    [conn("openai", ["gpt-5.5-codex"]), conn("anthropic-oauth", ["claude-fable-5"])],
    { statuses, hideInvalid: true },
  );
  expect(groups).toEqual([
    { label: "OpenAI", options: [{ value: "openai/gpt-5.5-codex-high", label: "gpt-5.5-codex-high", mono: true }] },
    { label: "Anthropic", options: [{ value: "claude-fable-5", label: "claude-fable-5", mono: true }] },
  ]);
});
