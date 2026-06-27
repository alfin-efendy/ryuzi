import type { GatewayDescriptor, RuntimeDescriptor, ProviderCatalog } from "./types";

export function makeCatalog(gateways: GatewayDescriptor[], runtimes: RuntimeDescriptor[]): ProviderCatalog {
  return {
    gateways,
    runtimes,
    gateway: (id) => gateways.find((g) => g.id === id),
    runtime: (id) => runtimes.find((r) => r.id === id),
  };
}
