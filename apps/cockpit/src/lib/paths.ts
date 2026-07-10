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

function hasUnsafeSegments(rel: string): boolean {
  return rel.split("/").some((seg) => seg === "" || seg === "." || seg === "..");
}

/** Heuristic: does an inline code span look like a repo-relative file path?
 *  Rejects absolutes, URLs, query/fragment strings, command-like text
 *  (whitespace before the first slash), and unsafe segments. Spaces INSIDE
 *  a segment are allowed ("docs/Design Notes.md"). */
export function looksLikeWorkspaceFilePath(text: string): boolean {
  if (!text) return false;
  if (text.startsWith("/")) return false;
  if (text.includes("://")) return false;
  if (text.includes("?") || text.includes("#")) return false;
  const slash = text.indexOf("/");
  if (slash <= 0) return false;
  if (/\s/.test(text.slice(0, slash))) return false;
  return !hasUnsafeSegments(text);
}

/** Resolve `text` to a clean workdir-relative posix path, or null when it
 *  cannot be a workspace file: URLs/query/fragment, absolute paths outside
 *  (or equal to) the workdir, and unsafe segments are rejected. Relative
 *  inputs pass through when their segments are safe. */
export function toWorkspaceRelativePath(text: string, workdir: string): string | null {
  if (!text || text.includes("://") || text.includes("?") || text.includes("#")) return null;
  const norm = text.replace(/\\/g, "/");
  const root = workdir.replace(/\\/g, "/").replace(/\/+$/, "");
  const absolute = norm.startsWith("/") || /^[A-Za-z]:\//.test(norm);
  if (absolute) {
    if (!root) return null;
    if (!norm.toLowerCase().startsWith(`${root.toLowerCase()}/`)) return null;
    const rel = norm.slice(root.length + 1);
    return rel && !hasUnsafeSegments(rel) ? rel : null;
  }
  return !hasUnsafeSegments(norm) ? norm : null;
}
