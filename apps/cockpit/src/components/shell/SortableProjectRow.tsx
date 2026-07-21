import type { ReactNode } from "react";
import { useSortable } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";

/** Wraps a project header row so the whole row is draggable — no grip. The
 *  PointerSensor's 5px activation distance (configured in Sidebar) preserves
 *  click-to-expand and the row's hover action buttons; only a real drag sorts. */
export function SortableProjectRow({ id, children }: { id: string; children: () => ReactNode }) {
  const { listeners, setNodeRef, transform, transition, isDragging } = useSortable({ id });
  const style = {
    transform: CSS.Transform.toString(transform),
    transition,
    opacity: isDragging ? 0.4 : 1,
    touchAction: "none" as const,
    cursor: isDragging ? "grabbing" : undefined,
  };
  return (
    <div ref={setNodeRef} style={style} {...listeners}>
      {children()}
    </div>
  );
}
