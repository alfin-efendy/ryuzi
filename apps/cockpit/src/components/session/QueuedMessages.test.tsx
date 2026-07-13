import { afterEach, beforeEach, expect, mock, spyOn, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useNative } from "@/store-native";
import { commands } from "@/bindings";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";

const { QueuedMessages } = await import("./QueuedMessages");

const KEY = sessKey(LOCAL_RUNNER, "s1");
const loadQueue = mock(() => Promise.resolve());

afterEach(cleanup);
beforeEach(() => {
  useNative.setState({ queuedBySession: {}, loadQueue });
  loadQueue.mockClear();
});

test("loads and renders durable queued messages", () => {
  useNative.setState({ queuedBySession: { [KEY]: [{ id: "a", text: "first message" }, { id: "b", text: "second message" }] }, loadQueue });
  render(<QueuedMessages runnerId={LOCAL_RUNNER} sessionPk="s1" />);
  expect(loadQueue).toHaveBeenCalledWith(LOCAL_RUNNER, "s1");
  expect(screen.getByText("first message")).toBeTruthy();
  expect(screen.getByText("second message")).toBeTruthy();
});

test("removal invokes the backend and removes its durable message", async () => {
  useNative.setState({ queuedBySession: { [KEY]: [{ id: "a", text: "hello" }] }, loadQueue });
  const remove = spyOn(commands, "removeSessionMessage").mockResolvedValue({ status: "ok", data: true });
  render(<QueuedMessages runnerId={LOCAL_RUNNER} sessionPk="s1" />);

  fireEvent.click(screen.getByRole("button", { name: /remove from queue/i }));

  await waitFor(() => expect(remove).toHaveBeenCalledWith(LOCAL_RUNNER, "s1", "a"));
  await waitFor(() => expect(useNative.getState().queuedBySession[KEY]).toEqual([]));
  remove.mockRestore();
});

test("empty queue renders nothing", () => {
  const { container } = render(<QueuedMessages runnerId={LOCAL_RUNNER} sessionPk="s1" />);
  expect(container.firstChild).toBeNull();
});
