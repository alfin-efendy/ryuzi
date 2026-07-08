import { expect, test } from "bun:test";
import { groupModelOptions, withLeadingOption } from "./model-groups";
import type { CatalogEntry, ConnectionInfo } from "../bindings";

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
