import { expect, test } from "bun:test";
import { DndContext, PointerSensor, KeyboardSensor } from "@dnd-kit/core";
import { SortableContext, arrayMove, useSortable, verticalListSortingStrategy, sortableKeyboardCoordinates } from "@dnd-kit/sortable";
import { CSS } from "@dnd-kit/utilities";
import { restrictToVerticalAxis } from "@dnd-kit/modifiers";

test("dnd-kit classic exports resolve", () => {
  expect(DndContext).toBeDefined();
  expect(PointerSensor).toBeDefined();
  expect(KeyboardSensor).toBeDefined();
  expect(SortableContext).toBeDefined();
  expect(useSortable).toBeDefined();
  expect(verticalListSortingStrategy).toBeDefined();
  expect(sortableKeyboardCoordinates).toBeDefined();
  expect(restrictToVerticalAxis).toBeDefined();
  expect(typeof arrayMove).toBe("function");
  expect(CSS.Transform).toBeDefined();
});
