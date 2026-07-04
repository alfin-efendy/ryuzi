import { useState } from "react";
import { Brain, ChevronDown, ChevronRight } from "lucide-react";
import { Button } from "@ryuzi/ui";
import { Markdown } from "./Markdown";

/** Collapsed-by-default internal reasoning: never confusable with the answer. */
export function ThoughtBlock({ markdown, streaming }: { markdown: string; streaming: boolean }) {
  const [open, setOpen] = useState(false);
  const Chevron = open ? ChevronDown : ChevronRight;
  const preview = markdown.trim().split("\n")[0].slice(0, 80);
  return (
    <div className="flex max-w-[82%] flex-col">
      <Button
        variant="ghost"
        size="xs"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="h-auto max-w-fit cursor-pointer gap-1.5 rounded-md px-1 py-0.5 font-semibold text-muted-foreground"
      >
        <Brain aria-hidden size={12} strokeWidth={2} className={streaming ? "animate-pulse" : ""} />
        {streaming ? "Thinking…" : "Thought"}
        <Chevron aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
        {!open && <span className="max-w-[360px] truncate font-normal">{preview}</span>}
      </Button>
      {open && (
        <div className="mt-1 border-l-2 border-border pl-3 text-[12.5px] leading-relaxed text-muted-foreground">
          <Markdown text={markdown} />
        </div>
      )}
    </div>
  );
}
