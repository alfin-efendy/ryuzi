import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { TodoPanel, todoBarSummary } from "./TodoPanel";
import { useNative } from "@/store-native";
import type { TodoItem } from "@/bindings";

// TodoPanel's mount effect calls loadTodos — replace it in the store so no IPC fires.
const loadTodos = mock(() => Promise.resolve());

function seed(todos: TodoItem[]) {
  useNative.setState({ todosBySession: { s1: todos }, loadTodos });
}

afterEach(cleanup);

test("todoBarSummary headlines the first in_progress item", () => {
  expect(
    todoBarSummary([
      { content: "step one", status: "completed" },
      { content: "step two", status: "in_progress" },
      { content: "step three", status: "pending" },
    ]),
  ).toEqual({ done: 1, total: 3, label: "step two" });
});

test("todoBarSummary falls back to the last completed item, then 'Plan'", () => {
  expect(
    todoBarSummary([
      { content: "step one", status: "completed" },
      { content: "step two", status: "completed" },
      { content: "step three", status: "pending" },
    ]).label,
  ).toBe("step two");
  expect(todoBarSummary([{ content: "later", status: "pending" }]).label).toBe("Plan");
});

test("renders nothing without todos", () => {
  useNative.setState({ todosBySession: {}, loadTodos });
  const { container } = render(<TodoPanel sessionPk="s1" running={false} />);
  expect(container.innerHTML).toBe("");
});

test("collapsed bar shows progress + current step only; click toggles the full list", () => {
  seed([
    { content: "step one", status: "completed" },
    { content: "step two", status: "in_progress" },
    { content: "step three", status: "pending" },
  ]);
  render(<TodoPanel sessionPk="s1" running />);
  // Collapsed: one line — count + active item; other items are not in the DOM.
  expect(screen.getByText("1/3")).toBeTruthy();
  expect(screen.getByText("step two")).toBeTruthy();
  expect(screen.queryByText("step one")).toBeNull();
  expect(screen.queryByText("step three")).toBeNull();
  // Expand: the popover lists every item.
  fireEvent.click(screen.getByRole("button", { name: /step two/ }));
  expect(screen.getByText("step one")).toBeTruthy();
  expect(screen.getByText("step three")).toBeTruthy();
  // Collapse again.
  fireEvent.click(screen.getByRole("button", { name: /step two/ }));
  expect(screen.queryByText("step three")).toBeNull();
});
