export class Registry<T> {
  private factories = new Map<string, () => T>();

  register(id: string, factory: () => T): void {
    this.factories.set(id, factory);
  }
  has(id: string): boolean {
    return this.factories.has(id);
  }
  create(id: string): T {
    const f = this.factories.get(id);
    if (!f) throw new Error(`unknown registry id: ${id}`);
    return f();
  }
  ids(): string[] {
    return [...this.factories.keys()];
  }
}
