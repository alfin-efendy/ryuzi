export type ContextQuery = {
  start: number;
  query: string;
};

export function activeContextQuery(draft: string): ContextQuery | null {
  const match = /(^|\s)@(\S*)$/.exec(draft);
  if (!match) return null;
  const start = match.index + match[1].length;
  const query = match[2];
  if (query && !query.includes("/") && !query.includes(".")) return null;
  return { start, query };
}

export function replaceActiveContextToken(draft: string, path: string): string {
  const active = activeContextQuery(draft);
  if (!active) return draft;
  return `${draft.slice(0, active.start)}@${path} `;
}

export function uniqueContextRefs(paths: string[]): string[] {
  return Array.from(new Set(paths.map((p) => p.trim()).filter(Boolean)));
}
