import type { Project } from "../bindings";
import { NATIVE_AGENT } from "../constants";

/**
 * The session header's agent line. Shows the PROJECT's pinned model — what
 * the composer picker writes and what the next turn will actually run —
 * falling back to the legacy agent default model, then a router-default label,
 * when unset.
 */
export function headerAgentLine(project: Project | undefined, defaultModel: string | null): string {
  return `${NATIVE_AGENT.name} · ${project?.model || defaultModel || "Router default"}`;
}
