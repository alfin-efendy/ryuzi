import type { CoreEvent } from "@harness/protocol";

export interface LiveSession { status?: string; lastText?: string }

export function reduceSessions(map: Map<string, LiveSession>, e: CoreEvent): void {
  const prev = map.get(e.sessionPk) ?? {};
  switch (e.kind) {
    case "session.created": map.set(e.sessionPk, { ...prev, status: "running" }); break;
    case "status":
    case "text": map.set(e.sessionPk, { ...prev, status: "running", lastText: e.text }); break;
    case "result": map.set(e.sessionPk, { ...prev, status: "idle" }); break;
    case "error": map.set(e.sessionPk, { ...prev, status: "error", lastText: e.message }); break;
    case "session.ended": map.set(e.sessionPk, { ...prev, status: "ended" }); break;
    default: break; // approval.requested etc. — no overlay change
  }
}
