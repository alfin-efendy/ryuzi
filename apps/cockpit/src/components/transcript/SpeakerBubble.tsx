import { agentColor } from "@/lib/agent-color";
import { Markdown } from "./Markdown";

/** A labeled bubble for a worker message in a group-chat run:
 *  a colored dot + the speaker's name above a bordered markdown body,
 *  distinct from the home agent's own AgentTurn bubbles. */
export function SpeakerBubble({ speaker, markdown }: { speaker: string; markdown: string }) {
  const color = agentColor(speaker);
  return (
    <div className="flex max-w-[82%] flex-col gap-1">
      <div className="flex items-center gap-1.5 text-[11px] font-medium" style={{ color }}>
        <span className="inline-block size-2 shrink-0 rounded-full" style={{ backgroundColor: color }} aria-hidden />
        {speaker}
      </div>
      <div className="rounded-lg border border-border/60 bg-muted/30 px-3 py-2 text-[13.5px] leading-relaxed text-foreground">
        <Markdown text={markdown} />
      </div>
    </div>
  );
}
