import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useNative } from "@/store-native";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";

const { QueuedMessages } = await import("./QueuedMessages");

const KEY = sessKey(LOCAL_RUNNER, "s1");
const loadQueue = mock(() => Promise.resolve());
const initialQueueState = {
  loadQueue: useNative.getState().loadQueue,
  removeQueueMessage: useNative.getState().removeQueueMessage,
  queuedBySession: useNative.getState().queuedBySession,
};
const realRemoveQueueMessage = initialQueueState.removeQueueMessage;

afterEach(() => {
  cleanup();
  useNative.setState(initialQueueState);
});
beforeEach(() => {
  useNative.setState({ queuedBySession: {}, loadQueue, removeQueueMessage: realRemoveQueueMessage });
  loadQueue.mockClear();
});

test("loads and renders durable queued messages", () => {
  useNative.setState({
    queuedBySession: {
      [KEY]: [
        { id: "a", text: "first message" },
        { id: "b", text: "second message" },
      ],
    },
    loadQueue,
  });
  render(<QueuedMessages runnerId={LOCAL_RUNNER} sessionPk="s1" />);
  expect(loadQueue).toHaveBeenCalledWith(LOCAL_RUNNER, "s1");
  expect(screen.getByText("first message")).toBeTruthy();
  expect(screen.getByText("second message")).toBeTruthy();
});

test("removal invokes the native queue action and updates its durable message", async () => {
  const removeQueueMessage = mock(() => Promise.resolve(true));
  useNative.setState({
    queuedBySession: { [KEY]: [{ id: "a", text: "hello" }] },
    loadQueue,
    removeQueueMessage,
  });
  render(<QueuedMessages runnerId={LOCAL_RUNNER} sessionPk="s1" />);

  fireEvent.click(screen.getByRole("button", { name: /remove from queue/i }));

  await waitFor(() => expect(removeQueueMessage).toHaveBeenCalledWith(LOCAL_RUNNER, "s1", "a"));
});

test("empty queue renders nothing", () => {
  const { container } = render(<QueuedMessages runnerId={LOCAL_RUNNER} sessionPk="s1" />);
  expect(container.firstChild).toBeNull();
});
