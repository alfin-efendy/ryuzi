export type AttrValue = string | number | boolean;
export type Attrs = Record<string, AttrValue>;

export interface Span {
  setAttribute(key: string, value: AttrValue): void;
  setError(message: string): void;
  end(): void;
}

export interface Telemetry {
  startSpan(name: string, attrs?: Attrs): Span;
  count(name: string, attrs?: Attrs): void;
  record(name: string, value: number, attrs?: Attrs): void;
  shutdown?(): Promise<void>;
}

class NoopSpan implements Span {
  setAttribute(): void {}
  setError(): void {}
  end(): void {}
}

export class NoopTelemetry implements Telemetry {
  startSpan(_name: string, _attrs?: Attrs): Span {
    return new NoopSpan();
  }
  count(_name: string, _attrs?: Attrs): void {}
  record(_name: string, _value: number, _attrs?: Attrs): void {}
}
