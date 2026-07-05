import { expect, test } from "bun:test";
import type { PluginInfo } from "@/bindings";
import { filterByCategory } from "./AppsView";

// AppsView itself pulls from four zustand stores (apps/runtimes/gateways/
// plugins) whose hydrate() calls would need a fairly heavy IPC mock to
// render meaningfully; the Catalog tab's one bit of real logic — filtering
// by category — is pure and exported for exactly this reason, so it's
// covered directly here instead.

function plugin(id: string, categories: string[]): PluginInfo {
  return {
    id,
    name: id,
    description: "",
    icon: null,
    categories,
    verified: true,
    experimental: false,
    enabled: false,
    source: "catalog",
    capabilities: ["connector"],
  };
}

const github = plugin("github", ["vcs", "issues"]);
const notion = plugin("notion", ["docs", "wiki", "productivity"]);
const sentry = plugin("sentry", ["observability"]);
const all = [github, notion, sentry];

test("filterByCategory passes every plugin through for the default all category", () => {
  expect(filterByCategory(all, "all")).toEqual(all);
});

test("filterByCategory keeps only plugins whose categories include the picked one", () => {
  expect(filterByCategory(all, "docs").map((p) => p.id)).toEqual(["notion"]);
});

test("filterByCategory matches a plugin tagged with several categories from any one of them", () => {
  expect(filterByCategory(all, "issues").map((p) => p.id)).toEqual(["github"]);
  expect(filterByCategory(all, "wiki").map((p) => p.id)).toEqual(["notion"]);
});

test("filterByCategory returns an empty list when nothing matches", () => {
  expect(filterByCategory(all, "sandbox")).toEqual([]);
});
