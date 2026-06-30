import { useState } from "react";
import { Textarea } from "@harness/ui";
import { composerMode } from "./composerMode";

export function Composer({
  onSubmit,
  running = false,
  onStop,
  disabled,
}: {
  onSubmit: (text: string) => void | Promise<void>;
  running?: boolean;
  onStop?: () => void;
  disabled?: boolean;
}) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const mode = composerMode(running ? "running" : "idle");

  const submit = async () => {
    const t = text.trim();
    if (!t) return;
    setSending(true);
    await onSubmit(t);
    setSending(false);
    setText("");
  };

  return (
    <div className="p-3 pt-2">
      <div className="mx-auto max-w-[720px] rounded-2xl border border-border bg-background p-3 shadow-sm">
        <Textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          onKeyDown={(e) => {
            if (mode === "stop" && e.key === "Escape") { onStop?.(); return; }
            if (mode === "send" && e.key === "Enter" && (e.metaKey || e.ctrlKey)) submit();
          }}
          placeholder="Message Claude…"
          className="min-h-[40px] resize-none border-0 bg-transparent p-0 shadow-none focus-visible:ring-0"
          disabled={disabled || sending}
        />
        <div className="mt-1.5 flex items-center gap-2">
          <span className="ml-auto text-[11px] text-muted-foreground">
            {mode === "stop" ? "session running — Esc to stop" : "⌘/Ctrl+Enter to send"}
          </span>
          {mode === "stop" ? (
            <button
              type="button"
              aria-label="Stop session"
              onClick={() => onStop?.()}
              className="flex h-8 w-8 items-center justify-center rounded-[10px] bg-foreground text-background"
            >
              <span className="h-[11px] w-[11px] rounded-[3px] bg-background" />
            </button>
          ) : (
            <button
              type="button"
              aria-label="Send"
              onClick={submit}
              disabled={disabled || sending}
              className="flex h-8 w-8 items-center justify-center rounded-[10px] bg-primary text-primary-foreground disabled:opacity-50"
            >
              <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.4" strokeLinecap="round"><path d="M12 19V5M5 12l7-7 7 7" /></svg>
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
