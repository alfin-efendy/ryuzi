import { test, expect } from "bun:test";
import { startApprovalServer } from "../src/core/approval-server";
import { runHook } from "../src/hook/pretooluse";

test("runHook approves through the real tokenized approval server", async () => {
  const server = startApprovalServer({ requestApproval: async () => "allow" });
  try {
    const res = await runHook({
      input: JSON.stringify({ tool_name: "Bash", tool_input: { command: "echo hi" } }),
      env: { HARNESS_APPROVAL_URL: server.url, HARNESS_SESSION_PK: "s1" },
      fetchFn: fetch,
    });
    expect(JSON.parse(res.stdout).hookSpecificOutput.permissionDecision).toBe("allow");
  } finally {
    server.stop();
  }
});

test("runHook denies through the real server when denied", async () => {
  const server = startApprovalServer({ requestApproval: async () => "deny" });
  try {
    const res = await runHook({
      input: JSON.stringify({ tool_name: "Bash", tool_input: {} }),
      env: { HARNESS_APPROVAL_URL: server.url, HARNESS_SESSION_PK: "s1" },
      fetchFn: fetch,
    });
    expect(JSON.parse(res.stdout).hookSpecificOutput.permissionDecision).toBe("deny");
  } finally {
    server.stop();
  }
});
