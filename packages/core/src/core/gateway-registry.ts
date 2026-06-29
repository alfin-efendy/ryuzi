import type { Gateway } from "../gateways/types";

export class GatewayRegistry {
  private gws = new Map<string, Gateway>();
  register(gw: Gateway): void {
    this.gws.set(gw.id, gw);
  }
  get(id: string): Gateway | undefined {
    return this.gws.get(id);
  }
  has(id: string): boolean {
    return this.gws.has(id);
  }
  ids(): string[] {
    return [...this.gws.keys()];
  }
}
