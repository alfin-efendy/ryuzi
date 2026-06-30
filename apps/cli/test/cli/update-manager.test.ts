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
  expect(emitted[0]).toMatchObject({ kind: "notice", sessionPk: "s1" });
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
    fetchImpl: fakeFetch("v0.2.0"), // no update so the initial void tick() is a no-op
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

test("upgradeHint: npm-style execPath produces npm install hint", async () => {
  const settings = new SettingsStore(openDb(":memory:"));
  const t = target([{ sessionPk: "s1", projectId: "p1", status: "idle" }]);
  const um = new UpdateManager({
    cp: t.cp,
    settings,
    version: "0.2.0",
    execPath: "/usr/local/lib/node_modules/hrctl/bin/hr",
    compiled: true,
    fetchImpl: fakeFetch("v0.3.0"),
  });
  await um.tick();
  expect((t.emitted[0] as { text: string }).text).toMatch(/npm i -g/);
});

test("upgradeHint: scoop-style execPath produces scoop update hint", async () => {
  const settings = new SettingsStore(openDb(":memory:"));
  const t = target([{ sessionPk: "s1", projectId: "p1", status: "idle" }]);
  const um = new UpdateManager({
    cp: t.cp,
    settings,
    version: "0.2.0",
    execPath: "C:\\Users\\me\\scoop\\apps\\harness-router\\current\\hr.exe",
    compiled: true,
    fetchImpl: fakeFetch("v0.3.0"),
  });
  await um.tick();
  expect((t.emitted[0] as { text: string }).text).toMatch(/scoop update/);
});

test("upgradeHint: install.sh path produces curl/install.sh hint", async () => {
  const settings = new SettingsStore(openDb(":memory:"));
  const t = target([{ sessionPk: "s1", projectId: "p1", status: "idle" }]);
  const um = new UpdateManager({
    cp: t.cp,
    settings,
    version: "0.2.0",
    execPath: "/home/me/.local/bin/hr",
    compiled: true,
    home: "/home/me",
    fetchImpl: fakeFetch("v0.3.0"),
  });
  await um.tick();
  expect((t.emitted[0] as { text: string }).text).toMatch(/install\.sh|curl/);
});

test("upgradeHint: unknown execPath produces GitHub release hint", async () => {
  const settings = new SettingsStore(openDb(":memory:"));
  const t = target([{ sessionPk: "s1", projectId: "p1", status: "idle" }]);
  const um = new UpdateManager({
    cp: t.cp,
    settings,
    version: "0.2.0",
    execPath: "/some/unknown/path/hr",
    compiled: true,
    fetchImpl: fakeFetch("v0.3.0"),
  });
  await um.tick();
  expect((t.emitted[0] as { text: string }).text).toMatch(/GitHub release/);
});

test("auto mode on a self-applicable install triggers applyUpdate, not a notice", async () => {
  let applied: { tag: string } | undefined;
  const { um, emitted } = mgr({
    execPath: "/home/me/.local/bin/hr", // installsh → selfApplicable
    home: "/home/me",
    applyUpdate: async (info) => {
      applied = { tag: info.tag };
    },
  });
  await um.tick();
  expect(applied?.tag).toBe("v0.3.0");
  expect(emitted).toHaveLength(0); // applied, not announced
});

test("auto mode on a non-self-applicable install still notifies (no apply)", async () => {
  let applied = false;
  const { um, emitted } = mgr({
    execPath: "/opt/homebrew/bin/hr", // brew → notify-only
    applyUpdate: async () => {
      applied = true;
    },
  });
  await um.tick();
  expect(applied).toBe(false);
  expect(emitted).toHaveLength(1);
});

test("notify mode never applies even on a self-applicable install", async () => {
  let applied = false;
  const { um, settings, emitted } = mgr({
    execPath: "/home/me/.local/bin/hr",
    home: "/home/me",
    applyUpdate: async () => {
      applied = true;
    },
  });
  settings.set("auto_update", "notify");
  await um.tick();
  expect(applied).toBe(false);
  expect(emitted).toHaveLength(1);
});

test("interrupted sessions are excluded from notice broadcast", async () => {
  const settings = new SettingsStore(openDb(":memory:"));
  const t = target([
    { sessionPk: "s1", projectId: "p1", status: "idle" },
    { sessionPk: "s2", projectId: "p1", status: "running" },
    { sessionPk: "s3", projectId: "p1", status: "interrupted" },
    { sessionPk: "s4", projectId: "p1", status: "ended" },
  ]);
  const um = new UpdateManager({
    cp: t.cp,
    settings,
    version: "0.2.0",
    execPath: "/home/me/.local/bin/hr",
    compiled: true,
    home: "/home/me",
    fetchImpl: fakeFetch("v0.3.0"),
  });
  await um.tick();
  // only idle and running are included; interrupted and ended are excluded
  expect(t.emitted).toHaveLength(2);
  expect(t.emitted.map((e) => (e as { sessionPk: string }).sessionPk).sort()).toEqual(["s1", "s2"]);
});
