import { useState } from "react";
import { Textarea } from "@/components/ui/textarea";
import { Button } from "@/components/ui/button";

export function Composer({ onSubmit, disabled }: { onSubmit: (text: string) => void | Promise<void>; disabled?: boolean }) {
  const [text, setText] = useState("");
  const [sending, setSending] = useState(false);
  const submit = async () => {
    const t = text.trim();
    if (!t) return;
    setSending(true);
    await onSubmit(t);
    setSending(false);
    setText("");
  };
  return (
    <div className="flex gap-2 border-t border-zinc-200 p-3 dark:border-zinc-800">
      <Textarea
        value={text}
        onChange={(e) => setText(e.target.value)}
        onKeyDown={(e) => { if (e.key === "Enter" && (e.metaKey || e.ctrlKey)) submit(); }}
        placeholder="Message Claude…  (⌘/Ctrl+Enter to send)"
        className="min-h-[44px] flex-1 resize-none"
        disabled={disabled || sending}
      />
      <Button onClick={submit} disabled={disabled || sending}>{sending ? "Sending…" : "Send"}</Button>
    </div>
  );
}
