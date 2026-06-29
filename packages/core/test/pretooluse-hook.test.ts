import { test, expect } from "bun:test";
import { runHook } from "../src/hook/pretooluse";

const PRE = JSON.stringify({ hook_event_name: "PreToolUse", tool_name: "Bash", tool_input: { command: "x" } });
const okFetch = (decision: "allow" | "deny"): typeof fetch =>
  (async () =>
    new Response(JSON.stringify({ permissionDecision: decision }), {
      headers: { "content-type": "application/json" },
    })) as unknown as typeof fetch;

test("allow when the IPC allows", async () => {
  const r = await runHook({ input: PRE, env: { HARNESS_APPROVAL_URL: "http://x", HARNESS_SESSION_PK: "s1" }, fetchFn: okFetch("allow") });
  expect(JSON.parse(r.stdout)).toEqual({ hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "allow" } });
  expect(r.exitCode).toBe(0);
});

test("deny when the IPC denies", async () => {
  const r = await runHook({ input: PRE, env: { HARNESS_APPROVAL_URL: "http://x", HARNESS_SESSION_PK: "s1" }, fetchFn: okFetch("deny") });
  expect(JSON.parse(r.stdout).hookSpecificOutput.permissionDecision).toBe("deny");
});

test("fail-closed: missing env → deny", async () => {
  const r = await runHook({ input: PRE, env: {}, fetchFn: okFetch("allow") });
  expect(JSON.parse(r.stdout).hookSpecificOutput.permissionDecision).toBe("deny");
});

test("fail-closed: fetch throws → deny", async () => {
  const boom = (async () => {
    throw new Error("unreachable");
  }) as unknown as typeof fetch;
  const r = await runHook({ input: PRE, env: { HARNESS_APPROVAL_URL: "http://x", HARNESS_SESSION_PK: "s1" }, fetchFn: boom });
  expect(JSON.parse(r.stdout).hookSpecificOutput.permissionDecision).toBe("deny");
});

test("deny output has the exact documented shape", async () => {
  const r = await runHook({ input: PRE, env: {}, fetchFn: okFetch("allow") });
  expect(JSON.parse(r.stdout)).toEqual({ hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: "deny" } });
});

test("fail-closed: non-JSON response → deny", async () => {
  const badJson = (async () => new Response("not json")) as unknown as typeof fetch;
  const r = await runHook({ input: PRE, env: { HARNESS_APPROVAL_URL: "http://x", HARNESS_SESSION_PK: "s1" }, fetchFn: badJson });
  expect(JSON.parse(r.stdout).hookSpecificOutput.permissionDecision).toBe("deny");
});
