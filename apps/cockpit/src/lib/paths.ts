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
