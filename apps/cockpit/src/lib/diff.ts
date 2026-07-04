// Unified-diff parsing for the session Review tab. The renderer's DiffLine
// shape: [type, lineNumber | "", text].

export type DiffLine = ["hunk" | "ctx" | "add" | "del", number | "", string];

export type ReviewFile = {
  dir: string;
  name: string;
  add: number;
  del: number;
  lines: DiffLine[];
};

function splitPath(path: string): { dir: string; name: string } {
  const idx = path.lastIndexOf("/");
  if (idx === -1) return { dir: "", name: path };
  return { dir: path.slice(0, idx + 1), name: path.slice(idx + 1) };
}

/** Parse `git diff` (unified) output into per-file line lists. */
export function parseUnifiedDiff(diff: string): ReviewFile[] {
  const files: ReviewFile[] = [];
  let current: ReviewFile | null = null;
  let oldLine = 0;
  let newLine = 0;

  for (const raw of diff.split("\n")) {
    if (raw.startsWith("diff --git ")) {
      current = null; // path comes from the +++/--- headers
      continue;
    }
    if (raw.startsWith("+++ ")) {
      const path = raw.slice(4).replace(/^b\//, "").trim();
      const { dir, name } = splitPath(path === "/dev/null" ? "(deleted)" : path);
      current = { dir, name, add: 0, del: 0, lines: [] };
      files.push(current);
      continue;
    }
    if (raw.startsWith("--- ")) continue;
    if (!current) continue;

    if (raw.startsWith("@@")) {
      const m = /@@ -(\d+)(?:,\d+)? \+(\d+)(?:,\d+)? @@(.*)/.exec(raw);
      if (m) {
        oldLine = Number(m[1]);
        newLine = Number(m[2]);
        current.lines.push(["hunk", "", raw.trim()]);
      }
      continue;
    }
    if (raw.startsWith("+")) {
      current.lines.push(["add", newLine, raw.slice(1)]);
      current.add += 1;
      newLine += 1;
    } else if (raw.startsWith("-")) {
      current.lines.push(["del", oldLine, raw.slice(1)]);
      current.del += 1;
      oldLine += 1;
    } else if (raw.startsWith(" ")) {
      current.lines.push(["ctx", newLine, raw.slice(1)]);
      oldLine += 1;
      newLine += 1;
    }
    // "\ No newline at end of file" and other markers are skipped.
  }
  return files;
}
