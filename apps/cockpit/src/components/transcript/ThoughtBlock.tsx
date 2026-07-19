import { Brain } from "lucide-react";
import { Markdown } from "./Markdown";

/** Internal reasoning stays visually distinct from the answer, but its content
 * is always available while reading the transcript. */
export function ThoughtBlock({ markdown, streaming }: { markdown: string; streaming: boolean }) {
  return (
    <div className="flex max-w-[82%] flex-col">
      <div className="flex items-center gap-1.5 px-1 py-0.5 text-[12px] font-semibold text-muted-foreground">
        <Brain aria-hidden size={12} strokeWidth={2} className={streaming ? "animate-pulse" : ""} />
        {streaming ? "Thinking…" : "Thought"}
      </div>
      <div className="mt-1 border-l-2 border-border pl-3 text-[12.5px] leading-relaxed text-muted-foreground">
        <Markdown text={markdown} />
      </div>
    </div>
  );
}
