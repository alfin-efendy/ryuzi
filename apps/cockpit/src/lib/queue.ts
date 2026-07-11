import type { ChatOptions } from "@/store";

/** One message waiting to be sent to a session. `options` is a snapshot of the
 *  per-message send options (context refs + attachments) taken at enqueue. */
export type QueuedMessage = {
  id: string;
  text: string;
  options: ChatOptions | null;
};

/** Append a message to a session's queue (new array; input untouched). */
export function enqueue(list: QueuedMessage[] | undefined, msg: QueuedMessage): QueuedMessage[] {
  return [...(list ?? []), msg];
}

/** Split the head off a queue. Empty/undefined → `{ head: null, rest: [] }`. */
export function dequeue(list: QueuedMessage[] | undefined): { head: QueuedMessage | null; rest: QueuedMessage[] } {
  if (!list || list.length === 0) return { head: null, rest: [] };
  const [head, ...rest] = list;
  return { head, rest };
}

/** Remove a message by id (new array; no-op if absent or undefined). */
export function removeById(list: QueuedMessage[] | undefined, id: string): QueuedMessage[] {
  return (list ?? []).filter((m) => m.id !== id);
}
