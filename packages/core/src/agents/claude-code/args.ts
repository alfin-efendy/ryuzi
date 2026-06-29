import type { AgentRunInput } from "../types";

export function buildHookSettings(hookBinPath: string): string {
  return JSON.stringify({
    hooks: { PreToolUse: [{ matcher: "*", hooks: [{ type: "command", command: `${process.execPath} ${hookBinPath}` }] }] },
  });
}

export function buildClaudeArgs(input: AgentRunInput, newSessionId: string): string[] {
  const args: string[] = ["-p", input.prompt, "--output-format", "stream-json", "--verbose"];
  if (input.resume) args.push("--resume", input.resume);
  else args.push("--session-id", newSessionId);
  if (input.model) args.push("--model", input.model);
  if (input.effort) args.push("--effort", input.effort);
  args.push("--permission-mode", input.permissionMode);
  if (input.permissionMode === "default" && input.approval) {
    args.push("--settings", buildHookSettings(input.approval.hookBinPath));
  }
  return args;
}
