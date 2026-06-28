// packages/client/test/ws.test.ts
import { test, expect } from "bun:test";
import { createControlPlaneClient } from "../src/index";
import type { ClientFrame, ServerFrame } from "@harness/protocol";

// Minimal in-memory WebSocket stub that records sent frames and lets the test push server frames.
class FakeWS {
  static last: FakeWS | undefined;
  onopen: (() => void) | null = null;
  onmessage: ((e: { data: string }) => void) | null = null;
  onclose: (() => void) | null = null;
  sent: ClientFrame[] = [];
  readyState = 0;
  constructor(public url: string) {
    FakeWS.last = this;
    queueMicrotask(() => {
      this.readyState = 1;
      this.onopen?.();
    });
  }
  send(data: string) {
    this.sent.push(JSON.parse(data) as ClientFrame);
  }
  close() {
    this.readyState = 3;
    this.onclose?.();
  }
  push(frame: ServerFrame) {
    this.onmessage?.({ data: JSON.stringify(frame) });
  }
}

function ticketFetch(): typeof fetch {
  return (async (url: string) => {
    if (String(url).endsWith("/ws-ticket")) return new Response(JSON.stringify({ ticket: "tkt" }), { status: 200 });
    return new Response("nope", { status: 404 });
  }) as unknown as typeof fetch;
}

test("connect performs hello handshake and forwards events", async () => {
  const events: string[] = [];
  const client = createControlPlaneClient({
    baseUrl: "http://router.test",
    getToken: async () => "tok",
    fetchImpl: ticketFetch(),
    WebSocketImpl: FakeWS as unknown as typeof WebSocket,
    autoReconnect: false,
  });
  client.onEvent((e) => events.push(e.kind));
  await client.connect();
  FakeWS.last!.push({ t: "hello", connId: "c1" });
  FakeWS.last!.push({ t: "event", event: { kind: "status", sessionPk: "s1", text: "working" } });
  expect(client.connId).toBe("c1");
  expect(events).toEqual(["status"]);
});

test("onConnectionChange emits connecting then open on connect()", async () => {
  const states: string[] = [];
  const client = createControlPlaneClient({
    baseUrl: "http://router.test",
    getToken: async () => "tok",
    fetchImpl: ticketFetch(),
    WebSocketImpl: FakeWS as unknown as typeof WebSocket,
    autoReconnect: false,
  });
  client.onConnectionChange((s) => states.push(s));
  await client.connect();
  // FakeWS queues onopen via queueMicrotask; await a microtask turn to let it fire
  await Bun.sleep(0);
  expect(states).toEqual(["connecting", "open"]);
});

test("approval request is delivered and resolveApproval sends a frame", async () => {
  const reqs: string[] = [];
  const client = createControlPlaneClient({
    baseUrl: "http://router.test",
    getToken: async () => "tok",
    fetchImpl: ticketFetch(),
    WebSocketImpl: FakeWS as unknown as typeof WebSocket,
    autoReconnect: false,
  });
  client.onApprovalRequest((r) => reqs.push(r.requestId));
  await client.connect();
  FakeWS.last!.push({ t: "approval.request", requestId: "r1", sessionPk: "s1", tool: "Bash", summary: "Bash: ls", timeoutMs: 1000 });
  client.resolveApproval("r1", "allow");
  expect(reqs).toEqual(["r1"]);
  expect(FakeWS.last!.sent).toEqual([{ t: "approval.resolve", requestId: "r1", decision: "allow" }]);
});
