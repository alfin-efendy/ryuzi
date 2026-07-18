import { expect, test } from "bun:test";
import type { AgentSummaryInfo, SearchEntryInfo } from "@/bindings";
import {
  activeContextQuery,
  contextPickerGroups,
  flattenContextPickerGroups,
  replaceActiveContextToken,
  uniqueContextRefs,
  type ContextPickerGroup,
  type ContextPickerProject,
} from "./composer-context";

test("parses every terminal whitespace-delimited @ token, including empty and bare-word queries", () => {
  expect(activeContextQuery("@", 1)).toEqual({ start: 0, end: 1, query: "" });
  expect(activeContextQuery("review @ada", 11)).toEqual({ start: 7, end: 11, query: "ada" });
  expect(activeContextQuery("review @src/views", 18)).toEqual({ start: 7, end: 18, query: "src/views" });
});

test("rejects an email-shaped @ token and a non-terminal @ token", () => {
  expect(activeContextQuery("email me@work", 13)).toBeNull();
  expect(activeContextQuery("review @src then", 17)).toBeNull();
});

test("replaces the active @ token with the selected value, preserving the tail after the caret", () => {
  expect(replaceActiveContextToken("review @src/vi", 14, "src/views/HomeView.tsx")).toBe("review @src/views/HomeView.tsx ");
  expect(replaceActiveContextToken("@", 1, "README.md")).toBe("@README.md ");
  expect(replaceActiveContextToken("review @src/vi and done", 14, "src/views/HomeView.tsx")).toBe(
    "review @src/views/HomeView.tsx  and done",
  );
});

test("uniqueContextRefs dedupes and drops blanks, preserving first-seen order", () => {
  expect(uniqueContextRefs(["src/a.ts", " ", "src/a.ts", "src/b.ts", ""])).toEqual(["src/a.ts", "src/b.ts"]);
});

const project: ContextPickerProject = { projectId: "proj1", name: "Acme Web" };

const ada = { id: "ada", name: "Ada", description: "Accessibility review", executable: true } as AgentSummaryInfo;
const lin = { id: "lin", name: "Lin", description: "Systems planner", executable: true } as AgentSummaryInfo;
const blocked = { id: "blocked", name: "Blocked", description: "Unavailable", executable: false } as AgentSummaryInfo;
const agents = [ada, lin, blocked];

const entries: SearchEntryInfo[] = [
  { path: "src/views", dir: true },
  { path: "src/views/HomeView.tsx", dir: false },
  { path: "src/lib", dir: true },
  { path: "README.md", dir: false },
  { path: "docs/plan.md", dir: false },
];

test("empty query returns Project + eligible agents (executable, non-primary) + all bounded search entries", () => {
  const groups = contextPickerGroups({ query: "", project, agents, primaryAgentId: "ada", entries });

  expect(groups).toEqual([
    { section: "project", label: "Project", items: [{ kind: "project", id: "proj1", name: "Acme Web" }] },
    { section: "agents", label: "Agents", items: [{ kind: "agent", agent: lin }] },
    {
      section: "folders",
      label: "Folders",
      items: [
        { kind: "workspace", path: "src/lib", dir: true },
        { kind: "workspace", path: "src/views", dir: true },
      ],
    },
    {
      section: "files",
      label: "Files",
      items: [
        { kind: "workspace", path: "docs/plan.md", dir: false },
        { kind: "workspace", path: "README.md", dir: false },
        { kind: "workspace", path: "src/views/HomeView.tsx", dir: false },
      ],
    },
  ]);
});

test("filters project, agents, and workspace entries case-insensitively across sections, hiding empty groups", () => {
  const groups = contextPickerGroups({ query: "LI", project, agents, primaryAgentId: "ada", entries });

  expect(groups.map((g) => g.section)).toEqual(["agents", "folders"]);
  expect(groups).toEqual([
    { section: "agents", label: "Agents", items: [{ kind: "agent", agent: lin }] },
    { section: "folders", label: "Folders", items: [{ kind: "workspace", path: "src/lib", dir: true }] },
  ]);
});

test("matches project name case-insensitively and hides sections with no matches", () => {
  const groups = contextPickerGroups({ query: "acme", project, agents, primaryAgentId: "ada", entries });

  expect(groups).toEqual([{ section: "project", label: "Project", items: [{ kind: "project", id: "proj1", name: "Acme Web" }] }]);
});

test("excludes the primary agent and non-executable agents from the Agents section", () => {
  const groups = contextPickerGroups({ query: "", project: null, agents, primaryAgentId: "ada", entries: [] });

  expect(groups).toEqual([{ section: "agents", label: "Agents", items: [{ kind: "agent", agent: lin }] }]);
});

test("groups directories under Folders and non-directories under Files, sorted deterministically", () => {
  const groups = contextPickerGroups({ query: "src", project, agents, primaryAgentId: "ada", entries });

  expect(groups).toEqual([
    {
      section: "folders",
      label: "Folders",
      items: [
        { kind: "workspace", path: "src/lib", dir: true },
        { kind: "workspace", path: "src/views", dir: true },
      ],
    },
    {
      section: "files",
      label: "Files",
      items: [{ kind: "workspace", path: "src/views/HomeView.tsx", dir: false }],
    },
  ]);
});

test("caps each section at 6 items with a deterministic sort", () => {
  const manyEntries: SearchEntryInfo[] = Array.from({ length: 8 }, (_, i) => ({ path: `src/dir${8 - i}`, dir: true }));
  const groups = contextPickerGroups({ query: "dir", project: null, agents: [], primaryAgentId: null, entries: manyEntries });

  expect(groups).toEqual([
    {
      section: "folders",
      label: "Folders",
      items: [
        { kind: "workspace", path: "src/dir1", dir: true },
        { kind: "workspace", path: "src/dir2", dir: true },
        { kind: "workspace", path: "src/dir3", dir: true },
        { kind: "workspace", path: "src/dir4", dir: true },
        { kind: "workspace", path: "src/dir5", dir: true },
        { kind: "workspace", path: "src/dir6", dir: true },
      ],
    },
  ]);
});

test("flattenContextPickerGroups preserves Project, Agents, Folders, Files order", () => {
  const groups = contextPickerGroups({ query: "", project, agents, primaryAgentId: "ada", entries });
  const flat = flattenContextPickerGroups(groups);

  expect(flat).toEqual([
    { kind: "project", id: "proj1", name: "Acme Web" },
    { kind: "agent", agent: lin },
    { kind: "workspace", path: "src/lib", dir: true },
    { kind: "workspace", path: "src/views", dir: true },
    { kind: "workspace", path: "docs/plan.md", dir: false },
    { kind: "workspace", path: "README.md", dir: false },
    { kind: "workspace", path: "src/views/HomeView.tsx", dir: false },
  ]);
});

test("flattenContextPickerGroups returns an empty array for no matches", () => {
  const groups: ContextPickerGroup[] = [];
  expect(flattenContextPickerGroups(groups)).toEqual([]);
});
