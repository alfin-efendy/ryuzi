import { test, expect } from "bun:test";
import { createControlPlaneClient } from "../src/index";
import type { RpcRequest } from "@harness/protocol";

function mockFetch(handler: (body: RpcRequest) => unknown): typeof fetch {
  return (async (_url: string, init?: RequestInit) => {
    const body = JSON.parse(String(init?.body)) as RpcRequest;
    const result = handler(body);
    return new Response(JSON.stringify({ id: body.id, ok: true, result }), { status: 200 });
  }) as unknown as typeof fetch;
}

test("listProjects issues a POST /rpc and returns the result", async () => {
  const calls: RpcRequest[] = [];
  const client = createControlPlaneClient({
    baseUrl: "http://router.test",
    getToken: async () => "tok",
    fetchImpl: mockFetch((b) => {
      calls.push(b);
      return [{ projectId: "p1", name: "demo", workdir: "/w", harness: "claude-code", permMode: "default" }];
    }),
  });
  const projects = await client.listProjects();
  expect(projects[0]?.projectId).toBe("p1");
  expect(calls[0]?.method).toBe("listProjects");
});

test("startSession sends params and surfaces server errors", async () => {
  const errFetch = (async (_u: string, init?: RequestInit) => {
    const body = JSON.parse(String(init?.body)) as RpcRequest;
    return new Response(JSON.stringify({ id: body.id, ok: false, error: "unknown project: x" }), { status: 200 });
  }) as unknown as typeof fetch;
  const client = createControlPlaneClient({ baseUrl: "http://router.test", getToken: async () => "tok", fetchImpl: errFetch });
  await expect(client.startSession({ projectId: "x", prompt: "hi" })).rejects.toThrow("unknown project: x");
});
