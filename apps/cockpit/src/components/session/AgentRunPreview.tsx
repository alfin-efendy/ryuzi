import { FileText, Wrench } from "lucide-react";
import type { AgentRunPreviewModel } from "@/lib/agent-runs";
import { toolCardHeader } from "@/lib/transcript";

function activityText(activity: AgentRunPreviewModel["activities"][number]): string {
  if (activity.type === "status") return activity.text;
  const { title, detail } = toolCardHeader(activity);
  return detail ? `${title} · ${detail}` : title;
}

/** A presentational, persisted-only glimpse of a child run. Keeping the
 * projection as a prop prevents a card from reading a mutable transcript
 * source and accidentally inventing live progress. */
export function AgentRunPreview({ preview }: { preview: AgentRunPreviewModel }) {
  const activities = preview.activities.slice(-3);
  if (activities.length === 0 && !preview.excerpt) return null;

  return (
    <section aria-label="Recent agent activity" className="mt-2 flex min-w-0 flex-col gap-1 border-t border-border/70 pt-2">
      {activities.map((activity) => {
        const Icon = activity.type === "tool" ? Wrench : FileText;
        return (
          <div key={activity.key} className="flex min-w-0 items-center gap-1.5 text-[11px] text-muted-foreground">
            <Icon aria-hidden size={11} strokeWidth={2} className="shrink-0" />
            <span className="truncate">{activityText(activity)}</span>
          </div>
        );
      })}
      {preview.excerpt && <p className="mb-0 line-clamp-2 text-[11.5px] leading-relaxed text-muted-foreground">{preview.excerpt}</p>}
    </section>
  );
}
