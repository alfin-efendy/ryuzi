// apps/router/test/approval-server.test.ts
import { test, expect } from "bun:test";
import { handleApprove, startApprovalServer, type ApproveBody } from "../src/core/approval-server";

const denier = { requestApproval: async (_b: ApproveBody) => "deny" as const };
const allower = { requestApproval: async (_b: ApproveBody) => "allow" as const };

test("handleApprove maps coordinator decision", async () => {
  expect(await handleApprove({ sessionPk: "s", tool: "Bash", input: {} }, denier)).toEqual({ permissionDecision: "deny" });
});

test("server does a real localhost roundtrip on the token URL", async () => {
  const server = startApprovalServer(allower);
  try {
    const res = await fetch(server.url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ sessionPk: "s1", tool: "Bash", input: { command: "x" } }),
    });
    expect(await res.json()).toEqual({ permissionDecision: "allow" });
  } finally {
    server.stop();
  }
});

test("server rejects a request without the token path (403)", async () => {
  const server = startApprovalServer(allower);
  try {
    const base = new URL(server.url).origin;
    const res = await fetch(`${base}/not-the-token`, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: JSON.stringify({ sessionPk: "s1", tool: "Bash", input: {} }),
    });
    expect(res.status).toBe(403);
  } finally {
    server.stop();
  }
});

test("server rejects non-POST", async () => {
  const server = startApprovalServer(allower);
  try {
    const res = await fetch(server.url, { method: "GET" });
    expect(res.status).toBe(405);
  } finally {
    server.stop();
  }
});

test("server rejects bad JSON with 400", async () => {
  const server = startApprovalServer(allower);
  try {
    const res = await fetch(server.url, {
      method: "POST",
      headers: { "content-type": "application/json" },
      body: "not json",
    });
    expect(res.status).toBe(400);
  } finally {
    server.stop();
  }
});
