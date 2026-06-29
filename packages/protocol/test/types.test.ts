import { test, expect } from "bun:test";
import { PERM_MODES, SESSION_STATUSES } from "../src/index";
import type {
  Project,
  Session,
  CoreEvent,
  ControlPlaneApi,
  AttachmentRef,
  StartSessionRequest,
  ContinueSessionRequest,
} from "../src/index";

test("protocol runtime enums are exported", () => {
  expect([...PERM_MODES]).toEqual(["default", "acceptEdits", "bypassPermissions"]);
  expect([...SESSION_STATUSES]).toEqual(["idle", "running", "interrupted", "ended"]);
});

test("protocol types compile with the expected shape", () => {
  const p: Project = { projectId: "p", name: "n", workdir: "/w", harness: "claude-code", permMode: "default" };
  const s: Session = { sessionPk: "s", projectId: "p", status: "idle" };
  const e: CoreEvent = { kind: "status", sessionPk: "s", text: "t" };
  // type-only reference to keep ControlPlaneApi exercised
  const api: Pick<ControlPlaneApi, "listProjects"> = { listProjects: () => [p] };
  expect(p.permMode).toBe("default");
  expect(s.status).toBe("idle");
  expect(e.kind).toBe("status");
  expect(api.listProjects()[0]?.projectId).toBe("p");
});

test("attachment ref + request attachments compile and carry fields", () => {
  const a: AttachmentRef = { name: "shot.png", url: "https://cdn/x", contentType: "image/png", size: 1234 };
  const start: StartSessionRequest = { projectId: "p", prompt: "hi", attachments: [a] };
  const cont: ContinueSessionRequest = { sessionPk: "s", prompt: "again", attachments: [a] };
  expect(start.attachments?.[0]?.name).toBe("shot.png");
  expect(cont.attachments?.[0]?.size).toBe(1234);
});
