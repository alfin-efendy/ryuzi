import { useEffect } from "react";
import { Button } from "@ryuzi/ui";
import { ListPlus, X } from "lucide-react";
import { useNative } from "@/store-native";
import { sessKey } from "@/lib/session-key";

/** The durable type-ahead queue for a session, shown above the composer.
 *  Renders nothing when the queue is empty. */
export function QueuedMessages({ runnerId, sessionPk }: { runnerId: string; sessionPk: string }) {
  const key = sessKey(runnerId, sessionPk);
  const queued = useNative((s) => s.queuedBySession[key]);
  const loadQueue = useNative((s) => s.loadQueue);
  const removeQueueMessage = useNative((s) => s.removeQueueMessage);

  useEffect(() => {
    void loadQueue(runnerId, sessionPk);
  }, [loadQueue, runnerId, sessionPk]);

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
            onClick={() => void removeQueueMessage(runnerId, sessionPk, m.id)}
            className="rounded-full"
          >
            <X aria-hidden size={13} strokeWidth={2} className="size-[13px]" />
          </Button>
        </div>
      ))}
    </div>
  );
}
