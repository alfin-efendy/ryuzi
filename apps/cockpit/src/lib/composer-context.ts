export type ContextQuery = {
  start: number;
  query: string;
};

export function activeContextQuery(draft: string): ContextQuery | null {
  const match = /(^|\s)@(\S*)$/.exec(draft);
  if (!match) return null;
  const start = match.index + match[1].length;
  if (start > 0 && /\w/.test(draft[start - 1] ?? "")) return null;
  return { start, query: match[2] };
}

export function replaceActiveContextToken(draft: string, path: string): string {
  const active = activeContextQuery(draft);
  if (!active) return draft;
  return `${draft.slice(0, active.start)}@${path} `;
}

export function uniqueContextRefs(paths: string[]): string[] {
  return Array.from(new Set(paths.map((p) => p.trim()).filter(Boolean)));
}
