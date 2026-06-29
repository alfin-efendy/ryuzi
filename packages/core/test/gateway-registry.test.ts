import { test, expect } from "bun:test";
import { GatewayRegistry } from "../src/core/gateway-registry";
import type { Gateway } from "../src/gateways/types";

function fakeGw(id: string): Gateway {
  return {
    id,
    start: async () => {},
    createWorkspace: async () => "w",
    createConversation: async () => "c",
    postStatus: async (t) => ({ surface: t, messageId: "m" }),
    editStatus: async () => {},
    postResult: async () => {},
    postError: async () => {},
    requestApproval: async () => ({ decision: "allow", actor: "x" }),
  };
}

test("register/get/has/ids on instances", () => {
  const r = new GatewayRegistry();
  expect(r.ids()).toEqual([]);
  const gw = fakeGw("discord");
  r.register(gw);
  expect(r.has("discord")).toBe(true);
  expect(r.get("discord")).toBe(gw); // same instance back
  expect(r.ids()).toEqual(["discord"]);
  expect(r.get("nope")).toBeUndefined();
});
