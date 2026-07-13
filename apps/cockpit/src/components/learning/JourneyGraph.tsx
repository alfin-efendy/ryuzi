import { useMemo, useState } from "react";
import { Button } from "@ryuzi/ui";
import type { JourneyMilestoneInfo } from "@/bindings";

export function JourneyGraph({ milestones }: { milestones: JourneyMilestoneInfo[] }) {
  const ordered = useMemo(() => [...milestones].sort((a, b) => a.timestamp.localeCompare(b.timestamp)), [milestones]);
  const [selected, setSelected] = useState<string | null>(null);
  return (
    <div className="rounded-lg border border-border bg-muted/20 p-3">
      {ordered.length === 0 ? (
        <p className="m-0 text-center text-xs text-muted-foreground">No learning milestones yet.</p>
      ) : (
        <ol className="m-0 flex list-none flex-col gap-2 p-0">
          {ordered.map((milestone, index) => (
            <li key={`${milestone.conceptId}:${milestone.timestamp}`} className="flex items-start gap-2 text-xs">
              <span className="mt-0.5 flex size-5 shrink-0 items-center justify-center rounded-full bg-primary/10 text-[10px] font-semibold text-primary">
                {index + 1}
              </span>
              <Button
                type="button"
                variant="ghost"
                className="h-auto min-w-0 flex-1 justify-start bg-transparent p-0 text-left"
                aria-pressed={selected === milestone.conceptId}
                onClick={() => setSelected(selected === milestone.conceptId ? null : milestone.conceptId)}
              >
                <span>
                  <span className="block font-medium">{milestone.title}</span>
                  <span className="text-[11px] text-muted-foreground">{new Date(milestone.timestamp).toLocaleString()}</span>
                </span>
              </Button>
            </li>
          ))}
        </ol>
      )}
    </div>
  );
}
