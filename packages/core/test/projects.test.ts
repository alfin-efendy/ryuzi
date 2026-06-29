import { test, expect } from "bun:test";
import type { Project } from "@harness/protocol";
import { openDb } from "../src/store/db";
import { ProjectsStore } from "../src/store/projects";

function sample(): Project {
  return {
    projectId: "p1",
    name: "foo",
    workdir: "/repos/foo",
    harness: "claude-code",
    permMode: "default",
    createdBy: "u1",
    createdAt: 1,
  };
}

test("insert + get + list", () => {
  const s = new ProjectsStore(openDb(":memory:"));
  s.insert(sample());
  expect(s.get("p1")?.name).toBe("foo");
  expect(s.get("p1")?.permMode).toBe("default");
  expect(s.list().length).toBe(1);
});

test("bind + resolveByWorkspace", () => {
  const s = new ProjectsStore(openDb(":memory:"));
  s.insert(sample());
  s.bind("discord", "chan-1", "p1");
  expect(s.resolveByWorkspace("discord", "chan-1")?.projectId).toBe("p1");
  expect(s.resolveByWorkspace("discord", "nope")).toBeUndefined();
});

test("one project can bind to multiple gateways/workspaces", () => {
  const s = new ProjectsStore(openDb(":memory:"));
  s.insert(sample());
  s.bind("discord", "chan-1", "p1");
  s.bind("slack", "C999", "p1");
  expect(s.resolveByWorkspace("discord", "chan-1")?.projectId).toBe("p1");
  expect(s.resolveByWorkspace("slack", "C999")?.projectId).toBe("p1");
});
