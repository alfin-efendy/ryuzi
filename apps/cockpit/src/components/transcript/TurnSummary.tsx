import { useState } from "react";
import { ChevronDown, ChevronRight } from "lucide-react";
import { Button } from "@ryuzi/ui";
import { formatTurnDuration, type Group } from "@/lib/transcript";
import { ThoughtBlock } from "./ThoughtBlock";
import { ActivityCluster } from "./ToolChip";

/** A completed turn's collapsed work: "Worked for 36s" + chevron, expanding
 *  to the same thought/activity rendering the live view streams. */
export function TurnSummary({ groups, durationMs }: { groups: Group[]; durationMs: number | null }) {
  const [open, setOpen] = useState(false);
  const Chevron = open ? ChevronDown : ChevronRight;
  const duration = formatTurnDuration(durationMs);
  return (
    <div className="flex flex-col">
      <Button
        variant="ghost"
        size="xs"
        aria-expanded={open}
        onClick={() => setOpen((v) => !v)}
        className="h-auto max-w-fit cursor-pointer gap-1.5 rounded-md px-1 py-0.5 font-semibold text-muted-foreground"
      >
        {duration ? `Worked for ${duration}` : "Worked"}
        <Chevron aria-hidden size={11} strokeWidth={2} className="size-[11px]" />
      </Button>
      {open && (
        <div className="mt-1.5 flex flex-col gap-2 border-l-2 border-border pl-3">
          {groups.map((g) =>
            g.type === "activity" ? (
              <ActivityCluster key={g.key} items={g.items} />
            ) : g.type === "thought" ? (
              <ThoughtBlock key={g.key} markdown={g.markdown} streaming={false} />
            ) : null,
          )}
        </div>
      )}
    </div>
  );
}
