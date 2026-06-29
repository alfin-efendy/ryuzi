import type { AgentEvent } from "../types";

function toolSummary(name: string, input: unknown): string {
  const obj = (input ?? {}) as Record<string, unknown>;
  if (name === "Bash" && typeof obj.command === "string") return `Bash: ${obj.command.slice(0, 80)}`;
  const target = obj.file_path ?? obj.path ?? obj.pattern;
  return typeof target === "string" ? `${name}: ${target}` : name;
}

export function parseLine(line: string): AgentEvent[] {
  let d: Record<string, unknown>;
  try {
    d = JSON.parse(line) as Record<string, unknown>;
  } catch {
    return [];
  }

  if (d.type === "system") {
    return d.subtype === "init" ? [{ type: "init", sessionId: String(d.session_id ?? "") }] : [];
  }

  if (d.type === "assistant") {
    const msg = d.message as { content?: Array<Record<string, unknown>> } | undefined;
    const out: AgentEvent[] = [];
    for (const b of msg?.content ?? []) {
      if (b.type === "text" && typeof b.text === "string") out.push({ type: "text", text: b.text });
      else if (b.type === "tool_use") out.push({ type: "status", text: toolSummary(String(b.name), b.input) });
    }
    return out;
  }

  if (d.type === "result") {
    if (d.is_error) return [{ type: "error", message: String(d.result ?? d.subtype ?? "error") }];
    return [{ type: "result", usage: d.usage, sessionId: d.session_id ? String(d.session_id) : undefined }];
  }

  return [];
}
