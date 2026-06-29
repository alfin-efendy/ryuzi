import { useStore } from "@/store";
import { Composer } from "./Composer";
import { ApprovalPrompt } from "./ApprovalPrompt";

export function SessionTranscript() {
  const { focusedSessionPk, transcripts, sessions, send } = useStore();

  if (!focusedSessionPk) {
    return <div className="flex h-full items-center justify-center text-sm text-zinc-500">Select or start a session.</div>;
  }
  const lines = transcripts[focusedSessionPk] ?? [];
  const session = sessions.find((s) => s.sessionPk === focusedSessionPk);

  return (
    <div className="flex h-full flex-col">
      <div className="flex items-center gap-2 border-b border-zinc-200 px-4 py-2 dark:border-zinc-800">
        <span className="text-sm font-medium">{session?.title ?? focusedSessionPk.slice(0, 8)}</span>
        <span className="text-xs text-zinc-500">{session?.status}</span>
        <span className="flex-1" />
        <button className="text-xs text-zinc-500 hover:text-zinc-900" onClick={() => useStore.getState().stop(focusedSessionPk)}>Stop</button>
        <button className="text-xs text-zinc-500 hover:text-red-600" onClick={() => useStore.getState().end(focusedSessionPk)}>End</button>
      </div>
      <div className="flex-1 space-y-2 overflow-auto p-4">
        {lines.length === 0 && <div className="text-sm text-zinc-500">Waiting for output…</div>}
        {lines.map((l, i) => (
          <div key={i} className={
            l.kind === "status" ? "text-xs font-mono text-zinc-500"
            : l.kind === "error" ? "rounded bg-red-50 p-2 text-sm text-red-700 dark:bg-red-950/40 dark:text-red-300"
            : "whitespace-pre-wrap text-sm"
          }>{l.text}</div>
        ))}
      </div>
      <ApprovalPrompt sessionPk={focusedSessionPk} />
      <Composer onSubmit={(t) => send(focusedSessionPk, t)} />
    </div>
  );
}
