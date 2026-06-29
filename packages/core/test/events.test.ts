import { test, expect } from "bun:test";
import { EventBus } from "../src/core/events";

test("EventBus delivers events to subscribers and unsubscribes", () => {
  const bus = new EventBus();
  const seen: string[] = [];
  const off = bus.subscribe((e) => {
    if (e.kind === "status") seen.push(e.text);
  });
  bus.emit({ kind: "status", sessionPk: "s1", text: "hello" });
  off();
  bus.emit({ kind: "status", sessionPk: "s1", text: "after-off" });
  expect(seen).toEqual(["hello"]);
});
