import type { SessionKind } from "@/bindings";

export type SessionRuntimeScope = "project" | "session" | null;

export function sessionRuntimeScope(kind: SessionKind | undefined, projectId: string | null): SessionRuntimeScope {
  if (projectId) return "project";
  return kind === "chat" ? "session" : null;
}
