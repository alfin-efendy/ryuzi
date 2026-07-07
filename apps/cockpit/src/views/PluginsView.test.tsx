import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";
import type { AppInfo, PluginInfo } from "@/bindings";

// PluginsView itself pulls from four zustand stores (apps/runtimes/gateways/
// plugins) whose hydrate() calls would need a fairly heavy IPC mock to
// render meaningfully; the Catalog tab's one bit of real logic — filtering
// by category — is pure and exported for exactly this reason, so it's
// covered directly here instead.

mock.module("@/store-apps", () => ({
  useApps: () => ({
    apps: [] as AppInfo[],
    loaded: true,
    hydrate: async () => {},
    toggleAgent: async () => {},
  }),
  agentAllowed: () => false,
}));

mock.module("@/store-runtimes", () => ({
  useRuntimes: (selector: (state: { runtimes: { id: string; name: string; color: string }[] }) => unknown) =>
    selector({ runtimes: [] }),
}));

mock.module("@/store-gateways", () => ({
  useGateways: (selector: (state: { gateways: { id: string; name: string }[] }) => unknown) => selector({ gateways: [] }),
}));

mock.module("@/store-plugins", () => ({
  usePlugins: () => ({
    plugins: [] as PluginInfo[],
    loaded: true,
    load: async () => {},
    setEnabled: async () => {},
  }),
  catalogPlugins: (plugins: PluginInfo[]) => plugins.filter((plugin) => plugin.source !== "builtin"),
}));

mock.module("@/store-nav", () => ({
  useNav: () => ({
    navigate: () => {},
  }),
}));

mock.module("@/components/modals/AddAppModal", () => ({
  AddAppModal: () => null,
}));

const { filterByCategory, PluginsView } = await import("./PluginsView");

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

afterEach(() => {
  cleanup();
});

test("renders the plugins heading and browse action", () => {
  render(<PluginsView />);

  expect(screen.getByRole("heading", { name: "Plugins" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Browse plugins" })).toBeTruthy();
});

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
