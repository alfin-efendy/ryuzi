export interface ToolInfo {
  found: boolean;
  path?: string;
  version?: string;
}

export type Runner = (cmd: string[]) => Promise<{ exitCode: number; stdout: string }>;

export const defaultRunner: Runner = async (cmd) => {
  try {
    const proc = Bun.spawn(cmd, { stdout: "pipe", stderr: "pipe" });
    const stdout = await new Response(proc.stdout).text();
    const exitCode = await proc.exited;
    return { exitCode, stdout: stdout.trim() };
  } catch {
    // Spawn throws (ENOENT) when the executable is not installed — treat that
    // as "not found" rather than crashing detection (e.g. `claude` absent).
    return { exitCode: 127, stdout: "" };
  }
};

export async function detectGit(run: Runner = defaultRunner): Promise<ToolInfo> {
  const res = await run(["git", "--version"]);
  if (res.exitCode !== 0) return { found: false };
  return { found: true, version: res.stdout.replace(/^git version\s*/i, "").trim() };
}

export async function detectClaude(run: Runner = defaultRunner): Promise<ToolInfo & { authenticated?: boolean }> {
  const res = await run(["claude", "--version"]);
  if (res.exitCode !== 0) return { found: false };
  const version = (res.stdout.match(/\d+\.\d+\.\d+/)?.[0] ?? res.stdout).trim();
  return { found: true, version, authenticated: undefined };
}
