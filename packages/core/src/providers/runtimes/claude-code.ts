import type { RuntimeDescriptor } from "../types";
import { ClaudeCodeHarness } from "../../agents/claude-code/index";
import { detectClaude } from "../../agents/detect";

export const claudeCodeRuntime: RuntimeDescriptor = {
  id: "claude-code",
  label: "Claude Code",
  description: "Anthropic's Claude Code CLI (uses your host login)",
  kind: "runtime",
  fields: [],
  detect: () => detectClaude(),
  build: () => new ClaudeCodeHarness(),
};
