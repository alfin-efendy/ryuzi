import { test, expect } from "bun:test";
import type { Session, CoreEvent } from "@harness/protocol";
import { openDb, SettingsStore } from "@harness/core";
import { UpdateManager, type NotifyTarget } from "../../src/cli/update-manager";

function fakeFetch(tag: string | null): typeof fetch {
  return (async () =>
    new Response(JSON.stringify(tag ? { tag_name: tag } : {}), {
      status: 200,
      headers: { "content-type": "application/json" },
    })) as unknown as typeof fetch;
}

function target(sessions: Session[]): { cp: NotifyTarget; emitted: CoreEvent[] } {
  const emitted: CoreEvent[] = [];
  return { cp: { listSessions: () => sessions, emit: (e) => emitted.push(e) }, emitted };
}

function mgr(over: Partial<ConstructorParameters<typeof UpdateManager>[0]> = {}) {
  const settings = new SettingsStore(openDb(":memory:"));
  const t = target([
    { sessionPk: "s1", projectId: "p1", status: "idle" },
    { sessionPk: "s2", projectId: "p1", status: "ended" },
  ]);
  const um = new UpdateManager({
    cp: t.cp,
    settings,
    version: "0.2.0",
    execPath: "/home/me/.local/bin/hr",
    compiled: true,
    home: "/home/me",
    fetchImpl: fakeFetch("v0.3.0"),
    ...over,
  });
  return { um, settings, ...t };
}

test("tick broadcasts a notice to non-ended sessions and records last_notified_version", async () => {
  const { um, settings, emitted } = mgr();
  await um.tick();
  expect(emitted).toHaveLength(1); // only s1 (s2 is ended)
  expect(emitted[0]).toMatchObject({ kind: "status", sessionPk: "s1" });
  expect((emitted[0] as { text: string }).text).toMatch(/0\.3\.0/);
  expect(settings.get("last_notified_version")).toBe("0.3.0");
});

test("tick dedupes — a second tick for the same version does not re-broadcast", async () => {
  const { um, emitted } = mgr();
  await um.tick();
  await um.tick();
  expect(emitted).toHaveLength(1);
});

test("no broadcast when there is no newer version", async () => {
  const { um, emitted } = mgr({ fetchImpl: fakeFetch("v0.2.0") });
  await um.tick();
  expect(emitted).toHaveLength(0);
});

test("mode off: tick is a no-op and start() arms no timer", async () => {
  let armed = 0;
  const { um, settings, emitted } = mgr({
    makeTimer: (_fn, _ms) => {
      armed++;
      return { stop: () => {} };
    },
  });
  settings.set("auto_update", "off");
  um.start();
  await um.tick();
  expect(emitted).toHaveLength(0);
  expect(armed).toBe(0); // start() must skip makeTimer entirely in off-mode
});

test("notify message names the install method's upgrade command", async () => {
  const { um, emitted } = mgr({ execPath: "/opt/homebrew/bin/hr" }); // brew
  await um.tick();
  expect((emitted[0] as { text: string }).text).toMatch(/brew upgrade/);
});

test("start() arms a timer via the injected makeTimer and stop() stops it", () => {
  let stops = 0;
  let armedMs = 0;
  const { um } = mgr({
    makeTimer: (_fn, ms) => {
      armedMs = ms;
      return {
        stop: () => {
          stops++;
        },
      };
    },
  });
  um.start();
  expect(armedMs).toBe(21600000); // default 6h
  um.stop();
  expect(stops).toBe(1);
});
