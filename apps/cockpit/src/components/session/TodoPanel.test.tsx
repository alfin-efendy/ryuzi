import { afterEach, describe, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { TodoPanel, todoStepSummary } from "./TodoPanel";
import { useNative } from "@/store-native";
import type { TodoItem } from "@/bindings";

const TODOS: TodoItem[] = [
  { content: "Map the four subsystems", status: "completed" },
  { content: "Write the modal plan", status: "in_progress" },
  { content: "Write the effort plan", status: "pending" },
];

// TodoPanel's mount effect calls loadTodos — replace it in the store so no IPC fires.
const loadTodos = mock(() => Promise.resolve());

afterEach(cleanup);

describe("todoStepSummary", () => {
  test("step = first in_progress (1-based), label = active item", () => {
    expect(todoStepSummary(TODOS)).toEqual({ step: 2, total: 3, done: 1, label: "Write the modal plan" });
  });
  test("no active item: step = done count, label = last completed", () => {
    const done = [
      { content: "a", status: "completed" },
      { content: "b", status: "completed" },
    ];
    expect(todoStepSummary(done)).toEqual({ step: 2, total: 2, done: 2, label: "b" });
  });
  test("empty and untouched lists", () => {
    expect(todoStepSummary([])).toEqual({ step: 0, total: 0, done: 0, label: "Plan" });
    expect(todoStepSummary([{ content: "x", status: "pending" }]).step).toBe(0);
  });
});

describe("TodoPanel", () => {
  test("expanded panel shows steps and the Step X / N footer; collapses to pill", () => {
    useNative.setState({ todosBySession: { s1: TODOS }, planCollapsed: {}, loadTodos });
    render(<TodoPanel sessionPk="s1" running={true} />);
    expect(screen.getByText("Write the modal plan")).toBeTruthy();
    expect(screen.getByText(/Step/)).toBeTruthy();
    // collapse via the header button
    fireEvent.click(screen.getByRole("button", { name: /collapse plan/i }));
    expect(screen.queryByText("Write the effort plan")).toBeNull(); // list hidden
    expect(screen.getByText(/Step 2\/3/)).toBeTruthy(); // pill summary
    // expand again via the pill
    fireEvent.click(screen.getByRole("button", { name: /expand plan/i }));
    expect(screen.getByText("Write the effort plan")).toBeTruthy();
  });

  test("renders nothing once settled with everything complete", () => {
    useNative.setState({
      todosBySession: { s1: TODOS.map((t) => ({ ...t, status: "completed" })) },
      planCollapsed: {},
      loadTodos,
    });
    const { container } = render(<TodoPanel sessionPk="s1" running={false} />);
    expect(container.innerHTML).toBe("");
  });
});
