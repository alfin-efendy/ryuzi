import { useEffect, useRef } from "react";
import { useStore } from "@/store";
import { Composer } from "./Composer";
import { ApprovalPrompt } from "./ApprovalPrompt";

export function SessionTranscript() {
  const { focusedSessionPk, transcripts, sessions, send, start, selectedProjectId, projects } = useStore();
  const scrollRef = useRef<HTMLDivElement>(null);
  const lines = focusedSessionPk ? (transcripts[focusedSessionPk] ?? []) : [];

  // biome-ignore lint/correctness/useExhaustiveDependencies: deps are intentional re-run triggers (scroll on new line / session switch).
  useEffect(() => {
    const el = scrollRef.current;
    if (el) el.scrollTop = el.scrollHeight;
  }, [lines.length, focusedSessionPk]);

  if (!focusedSessionPk) {
    if (selectedProjectId) {
      const project = projects.find((p) => p.projectId === selectedProjectId);
      return (
        <div className="flex h-full flex-col">
          <div className="flex h-[52px] shrink-0 items-center border-b border-border px-[18px] text-sm font-medium">
            New session on <span className="ml-1 font-semibold">{project?.name ?? selectedProjectId}</span>
          </div>
          <div className="flex flex-1 items-center justify-center px-4 text-center text-sm text-muted-foreground">
            Type a first message below to start a session.
          </div>
          <Composer onSubmit={(t) => start(selectedProjectId, t)} />
        </div>
      );
    }
    return (
      <div className="flex h-full items-center justify-center text-sm text-muted-foreground">
        Select a project (left) to start a session.
      </div>
    );
  }

  const session = sessions.find((s) => s.sessionPk === focusedSessionPk);
  const running = session?.status === "running";

  return (
    <div className="flex h-full flex-col">
      <div className="flex h-[52px] shrink-0 items-center gap-2.5 border-b border-border px-[18px]">
        <span className="text-sm font-semibold">{session?.title ?? focusedSessionPk.slice(0, 8)}</span>
        {session?.status && (
          <span className="flex items-center gap-1.5 rounded-full bg-blue-500/12 px-2 py-0.5 text-[11px] font-medium text-blue-600 dark:text-blue-300">
            <span className={`h-1.5 w-1.5 rounded-full ${running ? "bg-blue-500" : "bg-zinc-400"}`} />
            {session.status}
          </span>
        )}
      </div>

      <div ref={scrollRef} className="flex-1 overflow-auto py-6">
        <div className="mx-auto flex max-w-[720px] flex-col gap-4 px-6">
          {lines.length === 0 && <div className="text-sm text-muted-foreground">Waiting for output…</div>}
          {lines.map((l, i) =>
            l.kind === "user" ? (
              <div
                key={i}
                className="max-w-[80%] self-end rounded-2xl rounded-br-sm border border-border bg-muted px-3.5 py-2.5 text-[13.5px] leading-relaxed whitespace-pre-wrap"
              >
                {l.text}
              </div>
            ) : l.kind === "status" ? (
              <div key={i} className="pl-9 font-mono text-xs text-muted-foreground">{l.text}</div>
            ) : l.kind === "error" ? (
              <div key={i} className="ml-9 rounded-lg bg-destructive/10 p-2.5 text-sm text-destructive">{l.text}</div>
            ) : (
              <div key={i} className="flex gap-3">
                <div className="mt-0.5 flex h-6 w-6 shrink-0 items-center justify-center rounded-[7px] bg-primary text-primary-foreground">
                  <svg width="13" height="13" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round"><path d="M12 3a9 9 0 1 0 9 9" /><path d="M12 3v9l6 3" /></svg>
                </div>
                <div className="min-w-0 text-[13.5px] leading-relaxed whitespace-pre-wrap">{l.text}</div>
              </div>
            ),
          )}
        </div>
      </div>

      <ApprovalPrompt sessionPk={focusedSessionPk} />
      <Composer
        onSubmit={(t) => send(focusedSessionPk, t)}
        running={running}
        onStop={() => useStore.getState().stop(focusedSessionPk)}
      />
    </div>
  );
}
