// apps/ide/src/renderer/screens/SessionTranscript.tsx
import React, { useState } from "react";
import { useStore } from "../store";
import type { CoreEvent } from "@harness/protocol";
import { ApprovalCard } from "./ApprovalCard";

const EMPTY_EVENTS: CoreEvent[] = [];

export function SessionTranscript() {
  const activeSessionPk = useStore((s) => s.activeSessionPk);
  const connId = useStore((s) => s.connId);
  const projects = useStore((s) => s.projects);
  const events = useStore((s) => (s.activeSessionPk ? (s.transcripts[s.activeSessionPk] ?? EMPTY_EVENTS) : EMPTY_EVENTS));
  const pendingApprovals = useStore((s) => s.pendingApprovals);
  const [prompt, setPrompt] = useState("");

  async function send() {
    const text = prompt.trim();
    if (!text) return;
    setPrompt("");
    if (activeSessionPk) {
      await window.harness.continueSession({ sessionPk: activeSessionPk, prompt: text });
    } else if (projects[0] && connId) {
      const newSession = await window.harness.startSession({
        projectId: projects[0].projectId,
        prompt: text,
        surface: { gateway: "ide", conversationId: connId },
      });
      // Carry-forward: refresh session list so newly-started session appears in tree
      useStore.getState().setSessions(await window.harness.listSessions());
      useStore.getState().setActive(newSession.sessionPk);
    }
  }

  return (
    <div className="flex h-full flex-col" data-testid="transcript">
      <div className="min-h-0 flex-1 overflow-auto p-3 font-mono text-xs">
        {events.map((e, i) => (
          <div key={i} className={e.kind === "error" ? "text-red-500" : e.kind === "status" ? "text-muted-foreground" : ""}>
            {e.kind === "text" || e.kind === "status" ? e.text : e.kind === "error" ? e.message : `[${e.kind}]`}
          </div>
        ))}
        {pendingApprovals
          .filter((a) => a.sessionPk === activeSessionPk)
          .map((a) => (
            <ApprovalCard key={a.requestId} req={a} />
          ))}
      </div>
      <div className="flex gap-2 border-t p-2">
        <input
          className="flex-1 rounded border px-2 py-1 text-sm"
          value={prompt}
          onChange={(ev) => setPrompt(ev.target.value)}
          onKeyDown={(ev) => {
            if (ev.key === "Enter") void send();
          }}
          placeholder={activeSessionPk ? "continue…" : "start a session…"}
        />
        {activeSessionPk && (
          <>
            <button
              type="button"
              className="rounded border px-2 py-1 text-sm"
              onClick={() => void window.harness.stopSession(activeSessionPk)}
            >
              stop
            </button>
            <button
              type="button"
              className="rounded border px-2 py-1 text-sm"
              onClick={() => void window.harness.endSession(activeSessionPk)}
            >
              end
            </button>
          </>
        )}
      </div>
    </div>
  );
}
