import type { Gateway } from "../gateways/types";
import type { InboundRouter } from "../gateways/discord/index";
import type { Agent } from "../agents/types";
import type { ToolInfo } from "../agents/detect";

export interface ConfigField {
  key: string;
  label: string;
  help: string;
  example?: string;
  secret?: boolean;
  required?: boolean;
  control?: boolean; // set by pickers (enabled_*/default_runtime), not a free-text field
  type?: "string" | "int" | "enum";
  oneOf?: string[];
  default?: string;
}

export interface GatewayDescriptor {
  id: string;
  label: string;
  description: string;
  kind: "gateway";
  fields: ConfigField[];
  build(cfg: Record<string, string>, ctx: { router: InboundRouter }): Gateway;
}

export interface RuntimeDescriptor {
  id: string;
  label: string;
  description: string;
  kind: "runtime";
  fields: ConfigField[];
  detect(): Promise<ToolInfo & { authenticated?: boolean }>;
  build(): Agent;
}

export type ProviderDescriptor = GatewayDescriptor | RuntimeDescriptor;

export interface ProviderCatalog {
  gateways: GatewayDescriptor[];
  runtimes: RuntimeDescriptor[];
  gateway(id: string): GatewayDescriptor | undefined;
  runtime(id: string): RuntimeDescriptor | undefined;
}
