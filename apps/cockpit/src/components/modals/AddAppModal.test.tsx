import { expect, mock, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";

mock.module("@/store-apps", () => ({
  useApps: (selector: (state: { add: () => Promise<boolean> }) => unknown) => selector({ add: mock(async () => true) }),
}));

const { AddAppModal } = await import("./AddAppModal");

test("uses MCP server wording in the add modal title", () => {
  render(<AddAppModal onClose={() => {}} />);

  expect(screen.getByText("Add MCP server")).toBeTruthy();
  expect(screen.queryByText("Add app")).toBeNull();

  cleanup();
});
