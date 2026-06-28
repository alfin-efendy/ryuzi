export async function runHook(deps: {
  input: string;
  env: Record<string, string | undefined>;
  fetchFn: typeof fetch;
}): Promise<{ stdout: string; exitCode: number }> {
  let decision: "allow" | "deny" = "deny"; // fail-closed default
  try {
    const url = deps.env.HARNESS_APPROVAL_URL;
    const sessionPk = deps.env.HARNESS_SESSION_PK;
    const parsed = JSON.parse(deps.input) as { tool_name?: string; tool_input?: unknown };
    if (url && sessionPk) {
      // POST to the tokenized URL as-is — the token is the full path (see approval-server.ts)
      const res = await deps.fetchFn(url, {
        method: "POST",
        headers: { "content-type": "application/json" },
        body: JSON.stringify({ sessionPk, tool: parsed.tool_name ?? "", input: parsed.tool_input ?? {} }),
      });
      const out = (await res.json()) as { permissionDecision?: string };
      if (out.permissionDecision === "allow") decision = "allow";
    }
  } catch {
    decision = "deny";
  }
  return {
    stdout: JSON.stringify({ hookSpecificOutput: { hookEventName: "PreToolUse", permissionDecision: decision } }),
    exitCode: 0,
  };
}
