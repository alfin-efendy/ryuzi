import { afterEach, describe, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { TodoPanel, todoStepSummary } from "./TodoPanel";
import { useNative } from "@/store-native";
import type { TodoItem } from "@/bindings";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";

const KEY = sessKey(LOCAL_RUNNER, "s1");

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
    expect(todoStepSummary([])).toEqual({ step: 0, total: 0, done: 0, label: "TODO List" });
    expect(todoStepSummary([{ content: "x", status: "pending" }]).step).toBe(0);
  });
});

describe("TodoPanel", () => {
  test("compact TODO summary expands into the task list", () => {
    useNative.setState({ todosBySession: { [KEY]: TODOS }, planCollapsed: { [KEY]: true }, loadTodos });
    render(<TodoPanel runnerId={LOCAL_RUNNER} sessionPk="s1" running={true} />);

    expect(screen.getByRole("button", { name: "Expand TODO List" })).toBeTruthy();
    expect(screen.queryByText("Write the effort plan")).toBeNull();

    fireEvent.click(screen.getByRole("button", { name: "Expand TODO List" }));
    expect(screen.getByRole("button", { name: "Collapse TODO List" })).toBeTruthy();
    expect(screen.getByText("Write the effort plan")).toBeTruthy();
  });

  test("auto-collapses completed tasks into a completion summary", () => {
    useNative.setState({
      todosBySession: { [KEY]: TODOS.map((t) => ({ ...t, status: "completed" })) },
      planCollapsed: { [KEY]: false },
      loadTodos,
    });
    render(<TodoPanel runnerId={LOCAL_RUNNER} sessionPk="s1" running={false} />);

    expect(screen.getByText("All tasks completed")).toBeTruthy();
    expect(screen.getByRole("button", { name: "Expand TODO List" })).toBeTruthy();
  });
});
