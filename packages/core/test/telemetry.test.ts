import { test, expect } from "bun:test";
import { NoopTelemetry } from "../src/observability/types";
import { ConsoleTelemetry } from "../src/observability/console";

test("NoopTelemetry does nothing and never throws", () => {
  const t = new NoopTelemetry();
  const s = t.startSpan("x", { a: 1 });
  s.setAttribute("b", "y");
  s.setError("nope");
  s.end();
  t.count("c");
  t.record("h", 5);
  expect(true).toBe(true);
});

test("ConsoleTelemetry emits structured lines", () => {
  const lines: string[] = [];
  let clock = 100;
  const t = new ConsoleTelemetry(
    (l) => lines.push(l),
    () => clock,
  );
  t.count("session.run", { gateway: "discord" });
  t.record("dur", 42);
  const s = t.startSpan("harness.run", { session_pk: "s1" });
  s.setAttribute("branch", "harness/abc");
  clock = 350;
  s.end();
  const events = lines.map((l) => JSON.parse(l));
  expect(events[0]).toEqual({ kind: "count", name: "session.run", attrs: { gateway: "discord" } });
  expect(events[1]).toEqual({ kind: "record", name: "dur", value: 42, attrs: {} });
  expect(events[2].kind).toBe("span");
  expect(events[2].name).toBe("harness.run");
  expect(events[2].attrs).toEqual({ session_pk: "s1", branch: "harness/abc" });
  expect(events[2].durationMs).toBe(250);
});

test("ConsoleTelemetry span records error", () => {
  const lines: string[] = [];
  const t = new ConsoleTelemetry(
    (l) => lines.push(l),
    () => 0,
  );
  const s = t.startSpan("run");
  s.setError("boom");
  s.end();
  expect(JSON.parse(lines[0]!).error).toBe("boom");
});
