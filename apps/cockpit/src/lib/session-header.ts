import type { Project, RuntimeInfo } from "../bindings";

/**
 * The session header's agent line. Shows the PROJECT's pinned model — what
 * the composer picker writes and what the next turn will actually run —
 * falling back to the runtime card's default model, then its connection
 * label, when unset.
 */
export function headerAgentLine(agent: RuntimeInfo | undefined, project: Project | undefined): string {
  if (!agent) return "No agent detected";
  return `${agent.name} · ${project?.model || agent.model || agent.connection}`;
}
