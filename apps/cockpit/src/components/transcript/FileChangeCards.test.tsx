import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";

const gitDiff = mock(async () => ({ status: "ok" as const, data: "" }));
const sessionWorkdir = mock(async () => ({ status: "ok" as const, data: "/work/demo" }));
const revertFile = mock(async () => ({ status: "ok" as const, data: null }));

mock.module("@/bindings", () => ({
  commands: { gitDiff, sessionWorkdir, revertFile },
  events: { coreEventMsg: { listen: async () => () => {} } },
}));

const { FileChangeCards } = await import("./FileChangeCards");
const { useDiff } = await import("@/store-diff");
const { useNav } = await import("@/store-nav");

beforeEach(() => {
  useDiff.setState({
    bySession: { s1: { files: [], loading: false, error: null } },
    pendingReview: null,
  });
  useNav.setState({ rightOpen: false, rightTab: "file" });
});

afterEach(cleanup);

test("Review opens the right panel on the selected changed file", () => {
  render(
    <FileChangeCards
      sessionPk="s1"
      cards={[
        {
          path: "src/app.ts",
          kind: "edit",
        },
      ]}
    />,
  );

  fireEvent.click(screen.getByRole("button", { name: "Review" }));

  expect(useDiff.getState().pendingReview).toEqual({ sessionPk: "s1", path: "src/app.ts" });
  expect(useNav.getState().rightOpen).toBe(true);
  expect(useNav.getState().rightTab).toBe("review");
});
