import type { Telemetry, Span, Attrs } from "./types";

export class ConsoleTelemetry implements Telemetry {
  constructor(
    private sink: (line: string) => void = console.error,
    private now: () => number = Date.now,
  ) {}

  startSpan(name: string, attrs: Attrs = {}): Span {
    const start = this.now();
    const a: Attrs = { ...attrs };
    let error: string | undefined;
    const emit = () =>
      this.sink(JSON.stringify({ kind: "span", name, attrs: a, durationMs: this.now() - start, ...(error ? { error } : {}) }));
    return {
      setAttribute: (k, v) => {
        a[k] = v;
      },
      setError: (m) => {
        error = m;
      },
      end: () => emit(),
    };
  }
  count(name: string, attrs: Attrs = {}): void {
    this.sink(JSON.stringify({ kind: "count", name, attrs }));
  }
  record(name: string, value: number, attrs: Attrs = {}): void {
    this.sink(JSON.stringify({ kind: "record", name, value, attrs }));
  }
}
