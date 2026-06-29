// @harness/core — public surface. Consumers import ONLY from here, never deep paths.
export { buildDaemon } from "./daemon";
export { ControlPlane } from "./core/control-plane";
export { openDb } from "./store/db";
export { ProjectsStore } from "./store/projects";
export { SessionsStore } from "./store/sessions";
export { SettingsStore } from "./config/store";
export { expandHome } from "./config/paths";
export { catalog } from "./providers/catalog";
export { csv, missingRequiredSettings, isConfigured, requiredMissingFields } from "./config/required";
export { SETTING_DEFS, GLOBAL_FIELDS, allFields } from "./config/schema";
export { detectClaude, detectGit } from "./agents/detect";

export type { Agent, AgentEvent, AgentRunInput } from "./agents/types";
export type { ToolInfo, Runner } from "./agents/detect";
export type { ProviderCatalog, GatewayDescriptor, RuntimeDescriptor, ConfigField } from "./providers/types";
