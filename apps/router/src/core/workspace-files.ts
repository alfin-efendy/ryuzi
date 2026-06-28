import { closeSync, openSync, readSync, readdirSync, realpathSync, statSync } from "node:fs";
import { relative, resolve, sep } from "node:path";
import type { DirEntry, ReadFileResult } from "@harness/protocol";

const MAX_BYTES = 2 * 1024 * 1024; // 2 MB
const HIDDEN = new Set([".git"]);

// Resolve relPath against the worktree and confine it: the real (symlink-resolved)
// target must equal the real root or live under it. Rejects "..", absolute paths,
// symlinks escaping the worktree, and anything inside the .git subtree.
// Throws "not found" (without leaking absolute paths) when the path does not exist.
function confine(worktreeRoot: string, relPath: string): string {
  const root = realpathSync(worktreeRoot);
  let real: string;
  try {
    real = realpathSync(resolve(root, relPath));
  } catch {
    throw new Error("not found");
  }
  if (real !== root && !real.startsWith(root + sep)) {
    throw new Error("path escapes workspace");
  }
  // Block the .git subtree: reject if any segment of the path relative to root is ".git"
  const rel = relative(root, real);
  if (rel !== "" && rel.split(sep).some((seg) => seg === ".git")) {
    throw new Error("path escapes workspace");
  }
  return real;
}

export function listDir(worktreeRoot: string, relPath: string): DirEntry[] {
  const dir = confine(worktreeRoot, relPath);
  if (!statSync(dir).isDirectory()) throw new Error("not a directory");
  return readdirSync(dir, { withFileTypes: true })
    .filter((d) => !HIDDEN.has(d.name))
    .map((d) => ({ name: d.name, type: d.isDirectory() ? ("dir" as const) : ("file" as const) }))
    .sort((a, b) => (a.type !== b.type ? (a.type === "dir" ? -1 : 1) : a.name < b.name ? -1 : a.name > b.name ? 1 : 0));
}

export function readFile(worktreeRoot: string, relPath: string): ReadFileResult {
  const file = confine(worktreeRoot, relPath);
  const st = statSync(file);
  if (st.isDirectory()) throw new Error("is a directory");
  const truncated = st.size > MAX_BYTES;
  const buf = Buffer.alloc(Math.min(st.size, MAX_BYTES));
  const fd = openSync(file, "r");
  try {
    const n = readSync(fd, buf, 0, buf.length, 0);
    const slice = buf.subarray(0, n);
    const utf8 = slice.toString("utf8");
    const binary = slice.includes(0) || utf8.includes("�");
    return binary
      ? { content: slice.toString("base64"), encoding: "base64", binary: true, truncated }
      : { content: utf8, encoding: "utf8", binary: false, truncated };
  } finally {
    closeSync(fd);
  }
}
