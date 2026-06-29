import type { CoreEvent, Unsubscribe } from "@harness/protocol";

export class EventBus {
  private handlers = new Set<(e: CoreEvent) => void>();

  subscribe(handler: (e: CoreEvent) => void): Unsubscribe {
    this.handlers.add(handler);
    return () => {
      this.handlers.delete(handler);
    };
  }
  emit(e: CoreEvent): void {
    for (const h of [...this.handlers]) h(e);
  }
}
