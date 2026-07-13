import type { AgentRegistryInfo } from "@/bindings";

/** Resolve the configured default agent's model to the request value expected by session runtime fallbacks. */
export function defaultAgentModel(registry: AgentRegistryInfo | null): string | null {
  const model = registry?.agents.find((agent) => agent.id === registry.defaultAgentId)?.model;
  return model?.kind === "route" ? model.route : (model?.name ?? null);
}
