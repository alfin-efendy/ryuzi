import { useState } from "react";
import { Brain, ChevronDown, ChevronRight } from "lucide-react";
import { Markdown } from "./Markdown";

/** Collapsed-by-default internal reasoning: never confusable with the answer. */
export function ThoughtBlock({ markdown, streaming }: { markdown: string; streaming: boolean }) {
  const [open, setOpen] = useState(false);
  const Chevron = open ? ChevronDown : ChevronRight;
  const preview = markdown.trim().split("\n")[0].slice(0, 80);
  return (
    <div className="flex max-w-[82%] flex-col">
      <button
        type="button"
        onClick={() => setOpen((v) => !v)}
        className="flex max-w-fit cursor-pointer items-center gap-1.5 rounded-md border-none bg-transparent px-1 py-0.5 text-[11.5px] font-semibold text-muted-foreground hover:text-foreground"
      >
        <Brain aria-hidden size={12} strokeWidth={2} className={streaming ? "animate-pulse" : ""} />
        {streaming ? "Thinking…" : "Thought"}
        <Chevron aria-hidden size={11} strokeWidth={2} />
        {!open && <span className="max-w-[360px] truncate font-normal">{preview}</span>}
      </button>
      {open && (
        <div className="mt-1 border-l-2 border-border pl-3 text-[12.5px] leading-relaxed text-muted-foreground">
          <Markdown text={markdown} />
        </div>
      )}
    </div>
  );
}
