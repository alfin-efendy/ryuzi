import { afterEach, beforeEach, expect, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useStore } from "@/store";

const { QueuedMessages } = await import("./QueuedMessages");

afterEach(cleanup);
beforeEach(() => {
  useStore.setState({ queued: {} });
});

test("renders one row per queued message", () => {
  useStore.setState({
    queued: {
      s1: [
        { id: "a", text: "first message", options: null },
        { id: "b", text: "second message", options: null },
      ],
    },
  });
  render(<QueuedMessages sessionPk="s1" />);
  expect(screen.getByText("first message")).toBeTruthy();
  expect(screen.getByText("second message")).toBeTruthy();
});

test("× removes the message from the queue", () => {
  useStore.setState({ queued: { s1: [{ id: "a", text: "hello", options: null }] } });
  render(<QueuedMessages sessionPk="s1" />);
  fireEvent.click(screen.getByRole("button", { name: /remove from queue/i }));
  expect(useStore.getState().queued.s1).toEqual([]);
});

test("empty queue renders nothing", () => {
  const { container } = render(<QueuedMessages sessionPk="s1" />);
  expect(container.firstChild).toBeNull();
});
