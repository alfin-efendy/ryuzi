import { afterEach, expect, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import { useStore } from "@/store";
import { BlockCard } from "./BlockCard";

afterEach(cleanup);

test("renders the speaker's question", () => {
  render(<BlockCard taskId="ot-7" question="Which port?" speaker="build" />);
  expect(screen.getByText("build needs your input")).toBeTruthy();
  expect(screen.getByText("Which port?")).toBeTruthy();
});

test("Send answer is disabled until text is entered", () => {
  render(<BlockCard taskId="ot-7" question="Which port?" speaker="build" />);
  const send = screen.getByRole("button", { name: "Send answer" });
  expect(send.hasAttribute("disabled")).toBe(true);
  act(() => {
    fireEvent.change(screen.getByLabelText("Answer the worker"), { target: { value: "8080" } });
  });
  expect(send.hasAttribute("disabled")).toBe(false);
});

test("sending an answer calls orchAnswerBlock with the task id and trimmed text, then shows a sent state", async () => {
  const calls: Array<[string, string]> = [];
  const original = useStore.getState().orchAnswerBlock;
  useStore.setState({
    orchAnswerBlock: async (taskId, answer) => {
      calls.push([taskId, answer]);
    },
  });

  render(<BlockCard taskId="ot-7" question="Which port?" speaker="build" />);
  act(() => {
    fireEvent.change(screen.getByLabelText("Answer the worker"), { target: { value: "  use 8080  " } });
  });
  fireEvent.click(screen.getByRole("button", { name: "Send answer" }));

  // `orchAnswerBlock` resolves on a later microtask than this click, so the
  // "sent" state lands asynchronously — findByText polls (act-wrapped) until
  // it commits instead of asserting before the state update lands.
  expect(await screen.findByText("Answer sent — the worker will resume.")).toBeTruthy();
  expect(calls).toEqual([["ot-7", "use 8080"]]);
  expect(screen.queryByLabelText("Answer the worker")).toBeNull();

  useStore.setState({ orchAnswerBlock: original });
});
