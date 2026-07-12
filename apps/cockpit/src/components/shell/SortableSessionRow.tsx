import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { Button } from "@ryuzi/ui";
import { GripVertical } from "lucide-react";
import { SessionRow, type SessionRowProps } from "@/components/shell/SessionRow";
import { sessionKey } from "@/lib/session-key";

/** A pinned SessionRow made drag-sortable. The grip is the drag activator; the
 *  rest of the row keeps its normal click-to-open behavior. */
export function SortableSessionRow(props: Omit<SessionRowProps, "dragHandle">) {
  // Composite id (matches the SortableContext's `items`) — a bare `sessionPk`
  // would collide across runners.
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id: sessionKey(props.session) });
  const style = { transform: CSS.Transform.toString(transform), transition, opacity: isDragging ? 0.4 : 1 };
  const dragHandle = (
    <Button
      type="button"
      variant="ghost"
      size="icon-xs"
      title="Drag to reorder"
      aria-label="Drag to reorder"
      className="size-[22px] shrink-0 cursor-grab touch-none self-center rounded-sm text-muted-foreground opacity-0 group-hover:opacity-100"
      {...attributes}
      {...listeners}
    >
      <GripVertical aria-hidden size={12} strokeWidth={2} />
    </Button>
  );
  return (
    <div ref={setNodeRef} style={style}>
      <SessionRow {...props} dragHandle={dragHandle} />
    </div>
  );
}
