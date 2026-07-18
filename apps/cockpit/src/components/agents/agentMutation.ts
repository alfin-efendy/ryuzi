import type { AgentDetailInfo, AgentMutationInfo } from "@/bindings";

export function mutationFromDetail(detail: AgentDetailInfo): AgentMutationInfo {
  return {
    name: detail.summary.name,
    description: detail.summary.description,
    avatarColor: detail.summary.avatarColor,
    model: detail.summary.model,
    personality: detail.personality,
    permissionMode: detail.summary.permissionMode,
    permissionRules: detail.permissionRules,
    skills: detail.skills,
    nativeTools: detail.nativeTools,
    pluginTools: detail.pluginTools,
    apps: detail.apps,
    maxTurns: detail.maxTurns,
    maxToolRounds: detail.maxToolRounds,
  };
}
