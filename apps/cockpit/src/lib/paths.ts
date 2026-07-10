/** Last path segment, tolerant of both / and \ separators and trailing slashes. */
export function basename(path: string): string {
  return path.split(/[\\/]/).filter(Boolean).pop() ?? path;
}

const BADGES: Record<string, string> = {
  ts: "TS",
  tsx: "TS",
  js: "JS",
  jsx: "JS",
  rs: "RS",
  md: "MD",
  json: "{}",
  css: "CSS",
  html: "HTML",
  toml: "TOML",
};

/** Short badge for a dock tab, derived from the file extension. */
export function fileBadge(path: string): string {
  const name = basename(path);
  const dot = name.lastIndexOf(".");
  const ext = dot > 0 ? name.slice(dot + 1).toLowerCase() : "";
  if (!ext) return "FILE";
  return BADGES[ext] ?? ext.slice(0, 4).toUpperCase();
}

/** Parse a backticked chat token that looks like a file path (`src/a.ts`,
 *  `crates\core\lib.rs:42`, `C:\w\p\a.ts:10:5`). Requires a separator and a
 *  dotted final segment; strips an optional :line[:col] suffix. Returns null
 *  for URLs, tokens with whitespace, and everything else non-path-like. */
export function parsePathToken(token: string): { path: string; line: number | null } | null {
  if (/\s/.test(token) || token.includes("://")) return null;
  const m = token.match(/^(.+?)(?::(\d+)(?::\d+)?)?$/);
  if (!m) return null;
  const path = m[1];
  if (!/[\\/]/.test(path)) return null;
  const last = basename(path);
  if (!/\.[A-Za-z0-9]{1,8}$/.test(last)) return null;
  return { path, line: m[2] ? Number(m[2]) : null };
}

/** Join a workdir and a repo-relative posix path using the workdir's separator. */
export function joinPath(workdir: string, rel: string): string {
  const sep = workdir.includes("\\") ? "\\" : "/";
  const base = workdir.replace(/[\\/]+$/, "");
  return `${base}${sep}${rel.split("/").filter(Boolean).join(sep)}`;
}

/** Strip `workdir` from an absolute path, normalizing to forward slashes.
 *  Already-relative paths pass through normalized. */
export function toRepoRelative(path: string, workdir: string): string {
  const norm = path.replace(/\\/g, "/");
  const root = workdir.replace(/\\/g, "/").replace(/\/+$/, "");
  return root && norm.toLowerCase().startsWith(`${root.toLowerCase()}/`) ? norm.slice(root.length + 1) : norm;
}
