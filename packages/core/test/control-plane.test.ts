import { test, expect } from "bun:test";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";
import { SessionsStore } from "../src/store/sessions";
import { SettingsStore } from "../src/config/store";
import { ControlPlane } from "../src/core/control-plane";

function makeCp() {
  const db = openDb(":memory:");
  return new ControlPlane({
    projects: new ProjectsStore(db),
    sessions: new SessionsStore(db),
    settings: new SettingsStore(db),
    workdirRoot: "/root",
  });
}

test("exposes registries + read methods", () => {
  const cp = makeCp();
  expect(cp.harnesses.ids()).toEqual([]);
  expect(cp.gateways.ids()).toEqual([]);
  expect(cp.listProjects()).toEqual([]);
  expect(cp.listSessions()).toEqual([]);
});

test("emit reaches subscribers", () => {
  const cp = makeCp();
  const seen: string[] = [];
  cp.subscribe((e) => {
    if (e.kind === "error") seen.push(e.message);
  });
  cp.emit({ kind: "error", sessionPk: "s1", message: "boom" });
  expect(seen).toEqual(["boom"]);
});
