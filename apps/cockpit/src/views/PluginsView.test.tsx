import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AppInfo, PluginInfo, RegistryEntry } from "@/bindings";

const add = mock(async (_input: unknown) => true);
let mockApps: AppInfo[] = [];

mock.module("@/store-apps", () => ({
  useApps: () => ({
    apps: mockApps,
    loaded: true,
    hydrate: async () => {},
    add,
    toggleAgent: async () => {},
  }),
  agentAllowed: () => false,
}));

mock.module("@/store-runtimes", () => ({
  useRuntimes: (selector: (state: { runtimes: { id: string; name: string; color: string }[] }) => unknown) => selector({ runtimes: [] }),
}));

mock.module("@/store-gateways", () => ({
  useGateways: (selector: (state: { gateways: { id: string; name: string }[] }) => unknown) => selector({ gateways: [] }),
}));

const github = plugin("github", ["vcs", "issues"]);
const notion = plugin("notion", ["docs", "wiki", "productivity"]);
const builtin = {
  ...plugin("ollama", ["model-provider"]),
  source: "builtin" as const,
};

mock.module("@/store-plugins", () => ({
  usePlugins: () => ({
    plugins: [github, notion, builtin] as PluginInfo[],
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

const registrySearch = mock(async (_query: string | null, _cursor: string | null) => ({
  status: "ok" as const,
  data: {
    entries: [
      {
        ...entry("io.github/org/alpha", "1.1.0", {
          name: "Alpha Server",
          desc: "Registry alpha",
          installTarget: "@alpha/server@1.1.0",
          website: "https://alpha.example",
          isLatest: true,
        }),
        versions: [
          { version: "1.1.0", installTarget: "@alpha/server@1.1.0", website: "https://alpha.example", isLatest: true },
          { version: "1.0.0", installTarget: "@alpha/server@1.0.0", website: "https://alpha-old.example", isLatest: false },
        ],
      },
    ],
    nextCursor: null,
  },
}));

const listSkills = mock(async () => ({
  status: "ok" as const,
  data: [
    {
      id: "superpowers",
      name: "Superpowers",
      source: "superpowers",
      pluginId: null,
      installedAt: "2026-07-08T10:00:00Z",
      skillCount: 12,
    },
  ],
}));

type InstallSkillResponse =
  | {
      status: "ok";
      data: {
        id: string;
        name: string;
        source: string;
        pluginId: null;
        installedAt: string;
        skills: { id: string; name: string }[];
      };
    }
  | {
      status: "error";
      error: string;
    };

const installSkill = mock(
  async (_source: string): Promise<InstallSkillResponse> => ({
    status: "ok" as const,
    data: {
      id: "superpowers",
      name: "Superpowers",
      source: "superpowers",
      pluginId: null,
      installedAt: "2026-07-08T10:00:00Z",
      skills: [{ id: "superpowers:brainstorming", name: "brainstorming" }],
    },
  }),
);

const removeSkill = mock(async (_id: string) => ({
  status: "ok" as const,
  data: null,
}));

const refreshSkill = mock(async (_id: string) => ({
  status: "ok" as const,
  data: {
    id: "superpowers",
    name: "Superpowers",
    source: "superpowers",
    pluginId: null,
    installedAt: "2026-07-08T10:00:00Z",
    skills: [{ id: "superpowers:brainstorming", name: "brainstorming" }],
  },
}));

mock.module("@/bindings", () => ({
  commands: {
    registrySearch,
    listSkills,
    installSkill,
    removeSkill,
    refreshSkill,
  },
}));

const { useSkills } = await import("../store-skills");
const { filterByCategory, mergeRegistryEntries, PluginsView } = await import("./PluginsView");

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

function entry(
  id: string,
  version: string,
  options: {
    name?: string;
    desc?: string;
    installTarget?: string | null;
    website?: string | null;
    publisher?: string | null;
    kind?: string;
    isLatest?: boolean;
  } = {},
): RegistryEntry {
  return {
    id,
    name: options.name ?? "Server",
    desc: options.desc ?? "A server.",
    version,
    publisher: options.publisher ?? "Acme",
    kind: options.kind ?? "stdio",
    installTarget: options.installTarget ?? `npx ${id}@${version}`,
    website: options.website ?? null,
    versions: [
      {
        version,
        installTarget: options.installTarget ?? `npx ${id}@${version}`,
        website: options.website ?? null,
        isLatest: options.isLatest ?? false,
      },
    ],
  };
}

const all = [github, notion, builtin];

beforeEach(() => {
  mockApps = [];
  add.mockClear();
  registrySearch.mockClear();
  listSkills.mockClear();
  installSkill.mockClear();
  removeSkill.mockClear();
  refreshSkill.mockClear();
  useSkills.setState({ skills: [], loading: false, error: null });
});

afterEach(() => {
  cleanup();
});

test("renders the plugins heading and browse action", () => {
  render(<PluginsView />);

  expect(screen.getByRole("heading", { name: "Plugins" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Add MCP server" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Browse plugins" })).toBeTruthy();
  expect(screen.getByText("No plugins installed yet. Add an MCP server by hand or browse plugins.")).toBeTruthy();
});

test("access tab uses plugin wording for installed MCP server controls", () => {
  mockApps = [
    {
      id: "github",
      name: "GitHub",
      kind: "MCP server",
      initial: "G",
      color: "#111827",
      desc: "GitHub tools",
      transport: "stdio",
      command: "npx",
      args: ["-y", "@modelcontextprotocol/server-github"],
      url: null,
      scope: "global",
      scopeGateways: [],
      status: "connected",
      statusDetail: null,
      version: "1.0.0",
      publisher: "Acme",
      authKind: "none",
      authDetail: null,
      tools: [],
      agentAccess: [],
    },
  ];

  render(<PluginsView />);

  fireEvent.click(screen.getByRole("button", { name: "Access" }));

  expect(screen.getByText("Plugin")).toBeTruthy();
  expect(screen.getByText("Access here applies before per-tool permissions — a blocked agent never sees the plugin's tools.")).toBeTruthy();
  expect(screen.queryByText("App")).toBeNull();
  expect(screen.queryByRole("button", { name: "Add app" })).toBeNull();
});

test("browse combines catalog cards with live registry results from the same view", async () => {
  render(<PluginsView />);

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));

  await waitFor(() => expect(registrySearch).toHaveBeenCalledWith(null, null));
  expect(await screen.findByText("notion")).toBeTruthy();
  expect(await screen.findByText("Alpha Server")).toBeTruthy();
  expect(screen.getAllByText("Catalog").length).toBeGreaterThan(0);
  expect(screen.getAllByText("Registry").length).toBeGreaterThan(0);
});

test("registry install uses the selected version install target from browse", async () => {
  render(<PluginsView />);

  fireEvent.click(screen.getByRole("button", { name: "Browse" }));
  await screen.findByText("Alpha Server");

  fireEvent.click(screen.getByRole("combobox", { name: "Version for Alpha Server" }));
  fireEvent.click(await screen.findByRole("option", { name: "1.0.0" }));
  fireEvent.click(screen.getByRole("button", { name: "Install Alpha Server" }));

  await waitFor(() =>
    expect(add).toHaveBeenCalledWith(
      expect.objectContaining({
        name: "Alpha Server",
        version: "1.0.0",
        command: "npx",
        args: ["-y", "@alpha/server@1.0.0"],
      }),
    ),
  );
});

test("skills tab renders installed skills from listSkills", async () => {
  render(<PluginsView />);

  fireEvent.click(screen.getByRole("button", { name: "Skills" }));

  await waitFor(() => expect(listSkills).toHaveBeenCalledTimes(1));
  expect((await screen.findAllByText("Superpowers")).length).toBeGreaterThan(0);
  expect(screen.getByText("superpowers")).toBeTruthy();
  expect(screen.getByText("12 skills")).toBeTruthy();
  expect(screen.getByRole("button", { name: "Refresh Superpowers" })).toBeTruthy();
  expect(screen.getByRole("button", { name: "Remove Superpowers" })).toBeTruthy();
});

test("skills tab installs Superpowers from the curated action", async () => {
  render(<PluginsView />);

  fireEvent.click(screen.getByRole("button", { name: "Skills" }));
  await screen.findByText("Superpowers");

  fireEvent.click(screen.getByRole("button", { name: "Install Superpowers" }));

  await waitFor(() => expect(installSkill).toHaveBeenCalledWith("superpowers"));
});

test("manual skill install preserves the typed source after a failed attempt", async () => {
  installSkill.mockImplementationOnce(async () => ({
    status: "error" as const,
    error: "network down",
  }));

  render(<PluginsView />);

  fireEvent.click(screen.getByRole("button", { name: "Skills" }));
  await screen.findByText("Superpowers");

  const input = screen.getByRole("textbox", { name: "Skill source" }) as HTMLInputElement;
  fireEvent.change(input, { target: { value: "obra/superpowers" } });
  fireEvent.click(screen.getByRole("button", { name: "Install source" }));

  await waitFor(() => expect(installSkill).toHaveBeenCalledWith("obra/superpowers"));
  expect(input.value).toBe("obra/superpowers");
});

test("manual skill install clears the typed source after a successful attempt", async () => {
  render(<PluginsView />);

  fireEvent.click(screen.getByRole("button", { name: "Skills" }));
  await screen.findByText("Superpowers");

  const input = screen.getByRole("textbox", { name: "Skill source" }) as HTMLInputElement;
  fireEvent.change(input, { target: { value: "obra/superpowers" } });
  fireEvent.click(screen.getByRole("button", { name: "Install source" }));

  await waitFor(() => expect(installSkill).toHaveBeenCalledWith("obra/superpowers"));
  await waitFor(() => expect(input.value).toBe(""));
});

test("filterByCategory passes every plugin through for the default all category", () => {
  expect(filterByCategory(all, "all").map((item) => item.id)).toEqual(["github", "notion", "ollama"]);
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

test("mergeRegistryEntries de-dupes by id, keeps first-seen order, and uses isLatest-aware winner", () => {
  const pageOne: RegistryEntry[] = [
    { ...entry("io.github/org/alpha", "1.0.0", { desc: "alpha old", installTarget: "@alpha/server@1.0.0", isLatest: false }) },
    { ...entry("io.github/org/beta", "0.9.0", { installTarget: "@beta/server@0.9.0" }) },
  ];
  const pageTwo: RegistryEntry[] = [
    {
      ...entry("io.github/org/alpha", "1.1.0", {
        desc: "alpha latest",
        installTarget: "@alpha/server@1.1.0",
        website: "https://alpha.example",
        isLatest: true,
      }),
      versions: [{ version: "1.1.0", installTarget: "@alpha/server@1.1.0", website: "https://alpha.example", isLatest: true }],
    },
    { ...entry("io.github/org/gamma", "2.0.0", { installTarget: "@gamma/server@2.0.0" }) },
  ];

  const merged = mergeRegistryEntries(pageOne, pageTwo);

  expect(merged).toHaveLength(3);
  expect(merged.map((row) => row.id)).toEqual(["io.github/org/alpha", "io.github/org/beta", "io.github/org/gamma"]);

  const alpha = merged[0];
  expect(alpha.version).toBe("1.1.0");
  expect(alpha.desc).toBe("alpha latest");
  expect(alpha.installTarget).toBe("@alpha/server@1.1.0");
  expect(alpha.website).toBe("https://alpha.example");
  expect(alpha.versions.map((v) => v.version)).toEqual(["1.1.0", "1.0.0"]);
  expect(new Set(alpha.versions.map((v) => v.version)).size).toBe(2);
});

test("mergeRegistryEntries selects top-level winner by isLatest when newer version is not top-level", () => {
  const pageOne: RegistryEntry[] = [
    {
      ...entry("io.github/org/alpha", "2.0.0", {
        name: "Alpha Core",
        desc: "Original top-level name",
        installTarget: "@alpha/server@2.0.0",
        website: "https://old.example",
        isLatest: false,
      }),
      versions: [{ version: "2.0.0", installTarget: "@alpha/server@2.0.0", website: "https://old.example", isLatest: false }],
    },
  ];

  const pageTwo: RegistryEntry[] = [
    {
      ...entry("io.github/org/alpha", "2.0.0", {
        name: "Incoming name",
        desc: "Incoming top-level name",
        installTarget: "@alpha/server@2.0.0-new",
        website: "https://incoming.example",
        isLatest: false,
      }),
      versions: [
        { version: "2.0.0", installTarget: "@alpha/server@2.0.0-new", website: "https://incoming.example", isLatest: false },
        { version: "1.9.0", installTarget: "@alpha/server@1.9.0", website: "https://latest-only.example", isLatest: true },
      ],
    },
  ];

  const merged = mergeRegistryEntries(pageOne, pageTwo);

  expect(merged).toHaveLength(1);
  expect(merged[0].version).toBe("1.9.0");
  expect(merged[0].installTarget).toBe("@alpha/server@1.9.0");
  expect(merged[0].website).toBe("https://latest-only.example");
  expect(merged[0].name).toBe("Alpha Core");
  expect(merged[0].desc).toBe("Original top-level name");
  expect(merged[0].publisher).toBe("Acme");
  expect(merged[0].versions.map((v) => v.version)).toEqual(["1.9.0", "2.0.0"]);
});
