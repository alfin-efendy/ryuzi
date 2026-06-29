import type { GatewayDescriptor, RuntimeDescriptor, ProviderCatalog } from "./types";
import { discordGateway } from "./gateways/discord";
import { claudeCodeRuntime } from "./runtimes/claude-code";

export function makeCatalog(gateways: GatewayDescriptor[], runtimes: RuntimeDescriptor[]): ProviderCatalog {
  return {
    gateways,
    runtimes,
    gateway: (id) => gateways.find((g) => g.id === id),
    runtime: (id) => runtimes.find((r) => r.id === id),
  };
}

export const catalog: ProviderCatalog = makeCatalog([discordGateway], [claudeCodeRuntime]);
