import type { AgentSummaryInfo, SearchEntryInfo } from "@/bindings";

export type ContextQuery = {
  start: number;
  end: number;
  query: string;
};

/** Finds the terminal whitespace-delimited `@` token ending exactly at
 *  `caret`, if any. Matches an empty query (`@`), a bare word (`@ada`), and a
 *  path-shaped query (`@src/views`) alike — any run of non-whitespace
 *  characters after the `@` is a candidate context query as long as it is
 *  the last token before the caret. Rejects an `@` that isn't preceded by
 *  start-of-string or whitespace (so `me@work` inside an email is not a
 *  context query) and rejects a token that isn't at the caret (so typing
 *  `@src then` with the caret at the end is not an active query). */
export function activeContextQuery(text: string, caret: number): ContextQuery | null {
  const beforeCaret = text.slice(0, caret);
  const match = /(^|\s)@(\S*)$/.exec(beforeCaret);
  if (!match) return null;
  const start = match.index + match[1].length;
  return { start, end: caret, query: match[2] };
}

/** Replaces the active `@` token with `@${value} `, preserving whatever
 *  followed the caret (the "tail") unchanged. */
export function replaceActiveContextToken(text: string, caret: number, value: string): string {
  const active = activeContextQuery(text, caret);
  if (!active) return text;
  return `${text.slice(0, active.start)}@${value} ${text.slice(active.end)}`;
}

export function uniqueContextRefs(paths: string[]): string[] {
  return Array.from(new Set(paths.map((p) => p.trim()).filter(Boolean)));
}

export type ContextPickerSection = "project" | "agents" | "folders" | "files";

export type ContextPickerItem =
  | { kind: "project"; id: string; name: string }
  | { kind: "agent"; agent: AgentSummaryInfo }
  | { kind: "workspace"; path: string; dir: boolean };

export type ContextPickerGroup = {
  section: ContextPickerSection;
  label: string;
  items: ContextPickerItem[];
};

/** Minimal project shape the picker needs — deliberately narrower than the
 *  full `Project` binding so callers don't have to import the app store. */
export type ContextPickerProject = { projectId: string; name: string };

export type ContextPickerInput = {
  query: string;
  project: ContextPickerProject | null;
  agents: AgentSummaryInfo[];
  primaryAgentId: string | null;
  entries: SearchEntryInfo[];
};

const MAX_PER_SECTION = 6;

const SECTION_LABELS: Record<ContextPickerSection, string> = {
  project: "Project",
  agents: "Agents",
  folders: "Folders",
  files: "Files",
};

/** Groups matching context-picker candidates for the unified `@` menu,
 *  ordered Project, Agents, Folders, Files. Empty groups are omitted. Each
 *  section is capped at 6 items and workspace entries within a section are
 *  sorted by path for a deterministic, stable order. An empty query matches
 *  everything (bounded by the same per-section cap). */
export function contextPickerGroups(input: ContextPickerInput): ContextPickerGroup[] {
  const normalizedQuery = input.query.toLocaleLowerCase();
  const groups: ContextPickerGroup[] = [];

  if (input.project?.name.toLocaleLowerCase().includes(normalizedQuery)) {
    groups.push({
      section: "project",
      label: SECTION_LABELS.project,
      items: [{ kind: "project", id: input.project.projectId, name: input.project.name }],
    });
  }

  const matchingAgents = input.agents.filter(
    (agent) =>
      agent.executable &&
      agent.id !== input.primaryAgentId &&
      (agent.name.toLocaleLowerCase().includes(normalizedQuery) || agent.description.toLocaleLowerCase().includes(normalizedQuery)),
  );
  if (matchingAgents.length > 0) {
    groups.push({
      section: "agents",
      label: SECTION_LABELS.agents,
      items: matchingAgents.slice(0, MAX_PER_SECTION).map((agent) => ({ kind: "agent", agent }) as const),
    });
  }

  const matchingEntries = input.entries
    .filter((entry) => entry.path.toLocaleLowerCase().includes(normalizedQuery))
    .sort((a, b) => a.path.localeCompare(b.path));

  const folders = matchingEntries.filter((entry) => entry.dir);
  if (folders.length > 0) {
    groups.push({
      section: "folders",
      label: SECTION_LABELS.folders,
      items: folders.slice(0, MAX_PER_SECTION).map((entry) => ({ kind: "workspace", path: entry.path, dir: entry.dir }) as const),
    });
  }

  const files = matchingEntries.filter((entry) => !entry.dir);
  if (files.length > 0) {
    groups.push({
      section: "files",
      label: SECTION_LABELS.files,
      items: files.slice(0, MAX_PER_SECTION).map((entry) => ({ kind: "workspace", path: entry.path, dir: entry.dir }) as const),
    });
  }

  return groups;
}

/** Flattens grouped picker sections into a single ordered list, e.g. for
 *  keyboard navigation across the whole menu. */
export function flattenContextPickerGroups(groups: ContextPickerGroup[]): ContextPickerItem[] {
  return groups.flatMap((group) => group.items);
}
