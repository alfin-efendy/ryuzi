import { existsSync } from "node:fs";
import type { Agent, AgentEvent, AgentRunInput } from "../types";
import { buildClaudeArgs } from "./args";
import { parseLine } from "./parse";

export type ClaudeRunner = (
  args: string[],
  opts: { cwd: string; signal: AbortSignal; env?: Record<string, string> },
) => AsyncIterable<string>;

// Resolve the absolute `claude` binary so spawning does not depend on the daemon's PATH
// (e.g. a `harness start` shell that lacks ~/.local/bin). Falls back to known install dirs.
export function resolveClaudeBinary(): string {
  const fromPath = Bun.which("claude");
  if (fromPath) return fromPath;
  const home = process.env.HOME ?? "";
  for (const p of [`${home}/.local/bin/claude`, `${home}/.bun/bin/claude`, "/usr/local/bin/claude", "/opt/homebrew/bin/claude"]) {
    if (home && existsSync(p)) return p;
  }
  return "claude";
}

export const defaultClaudeRunner: ClaudeRunner = async function* (args, { cwd, signal, env }) {
  const proc = Bun.spawn([resolveClaudeBinary(), ...args], {
    cwd,
    stdout: "pipe",
    stderr: "pipe",
    signal,
    env: { ...process.env, ...(env ?? {}) },
  });
  const stderrPromise = new Response(proc.stderr).text();
  const decoder = new TextDecoder();
  let buf = "";
  for await (const chunk of proc.stdout) {
    buf += decoder.decode(chunk);
    let nl: number;
    while ((nl = buf.indexOf("\n")) >= 0) {
      const line = buf.slice(0, nl);
      buf = buf.slice(nl + 1);
      if (line.trim()) yield line;
    }
  }
  if (buf.trim()) yield buf;
  const code = await proc.exited;
  if (code !== 0) {
    const err = await stderrPromise;
    throw new Error(`claude exited ${code}: ${err.slice(0, 300)}`);
  }
};

export class ClaudeCodeHarness implements Agent {
  readonly id = "claude-code";
  constructor(private runner: ClaudeRunner = defaultClaudeRunner) {}

  async *run(input: AgentRunInput): AsyncIterable<AgentEvent> {
    const sessionId = input.resume ?? crypto.randomUUID();
    if (!input.resume) yield { type: "init", sessionId };
    const args = buildClaudeArgs(input, sessionId);
    const env: Record<string, string> =
      input.permissionMode === "default" && input.approval
        ? { HARNESS_APPROVAL_URL: input.approval.url, HARNESS_SESSION_PK: input.approval.sessionPk }
        : {};
    try {
      for await (const line of this.runner(args, { cwd: input.workdir, signal: input.signal, env })) {
        for (const ev of parseLine(line)) yield ev;
      }
    } catch (e) {
      yield { type: "error", message: e instanceof Error ? e.message : String(e) };
    }
  }
}
