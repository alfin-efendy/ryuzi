import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { SessionRow, type SessionRowProps } from "@/components/shell/SessionRow";
import { sessionKey } from "@/lib/session-key";

/** A SessionRow made drag-sortable by grabbing the row itself — no separate
 *  grip. The PointerSensor's 5px activation distance (configured in Sidebar)
 *  keeps a plain click as click-to-open; only a real drag starts sorting. */
export function SortableSessionRow(props: SessionRowProps) {
  // Composite id (matches the SortableContext's `items`) — a bare `sessionPk`
  // would collide across runners.
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id: sessionKey(props.session) });
  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.4 : 1,
    touchAction: "none" as const,
    cursor: isDragging ? "grabbing" : undefined,
  };
  return (
    <div ref={setNodeRef} style={style} {...attributes} {...listeners}>
      <SessionRow {...props} />
    </div>
  );
}
