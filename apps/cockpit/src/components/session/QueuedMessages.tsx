import { Button } from "@ryuzi/ui";
import { ListPlus, X } from "lucide-react";
import { useStore } from "@/store";

/** The pending type-ahead queue for a session, shown above the composer.
 *  Renders nothing when the queue is empty. */
export function QueuedMessages({ sessionPk }: { sessionPk: string }) {
  const queued = useStore((s) => s.queued[sessionPk]);
  const removeQueued = useStore((s) => s.removeQueued);
  if (!queued || queued.length === 0) return null;
  return (
    <div className="mx-auto mb-1.5 flex w-full max-w-3xl flex-col gap-1">
      {queued.map((m) => (
        <div
          key={m.id}
          className="flex items-center gap-2 rounded-lg border border-border bg-muted/40 px-3 py-1.5 text-[12.5px] text-muted-foreground"
        >
          <ListPlus aria-hidden size={13} strokeWidth={2} className="size-[13px] shrink-0" />
          <span className="min-w-0 flex-1 truncate">{m.text}</span>
          <Button
            variant="ghost"
            size="icon-sm"
            title="Remove from queue"
            onClick={() => removeQueued(sessionPk, m.id)}
            className="rounded-full"
          >
            <X aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
          </Button>
        </div>
      ))}
    </div>
  );
}
