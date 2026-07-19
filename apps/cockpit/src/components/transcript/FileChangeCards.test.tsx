import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen } from "@testing-library/react";
import { LOCAL_RUNNER, sessKey } from "@/lib/session-key";

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
    bySession: { [sessKey(LOCAL_RUNNER, "s1")]: { files: [], loading: false, error: null } },
    pendingReview: null,
  });
  useNav.setState({ rightOpen: false, rightTab: "file" });
});

afterEach(cleanup);

test("uses green and destructive highlights for write and delete changes", () => {
  const { container } = render(
    <FileChangeCards
      runnerId={LOCAL_RUNNER}
      sessionPk="s1"
      cards={[
        { path: "src/new.ts", kind: "write" },
        { path: "src/obsolete.ts", kind: "delete" },
      ]}
    />,
  );

  expect(screen.getByTitle("src/new.ts").closest("div")?.className).toContain("bg-green-500/5");
  expect(screen.getByTitle("src/obsolete.ts").closest("div")?.className).toContain("bg-destructive/5");
  expect(container.querySelector(".text-green-700")).toBeTruthy();
  expect(container.querySelector(".text-destructive")).toBeTruthy();
});
test("Review opens the right panel on the selected changed file", () => {
  render(
    <FileChangeCards
      runnerId={LOCAL_RUNNER}
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

  expect(useDiff.getState().pendingReview).toEqual({ runnerId: LOCAL_RUNNER, sessionPk: "s1", path: "src/app.ts" });
  expect(useNav.getState().rightOpen).toBe(true);
  expect(useNav.getState().rightTab).toBe("review");
});
