import type { ReactNode } from "react";
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { Button } from "@ryuzi/ui";
import { GripVertical } from "lucide-react";

/** Wraps a project header row so it can be dragged by a hover grip. */
export function SortableProjectRow({ id, children }: { id: string; children: (dragHandle: ReactNode) => ReactNode }) {
  const { attributes, listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id });
  const style = { transform: CSS.Transform.toString(transform), transition, opacity: isDragging ? 0.4 : 1 };
  const dragHandle = (
    <Button
      type="button"
      variant="ghost"
      size="icon-xs"
      title="Drag to reorder"
      aria-label="Drag to reorder project"
      className="size-[22px] shrink-0 cursor-grab touch-none self-center rounded-sm text-muted-foreground opacity-0 group-hover:opacity-100"
      {...attributes}
      {...listeners}
    >
      <GripVertical aria-hidden size={12} strokeWidth={2} />
    </Button>
  );
  return (
    <div ref={setNodeRef} style={style}>
      {children(dragHandle)}
    </div>
  );
}
