import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";
import type { AddAppInput } from "@/bindings";

// Mock the Tauri IPC boundary; the modal's submit path isn't exercised by the
// test below, but keeping this mock (rather than a real invoke) matches the
// repo convention and guards against a real Tauri call if a submit test is
// added later.
const addApp = mock(async (_input: AddAppInput) => ({ status: "ok" as const, data: [] }));

mock.module("@/bindings", () => ({
  commands: {
    addApp,
  },
}));

const { useApps } = await import("@/store-apps");
const { AddAppModal } = await import("./AddAppModal");

beforeEach(() => {
  addApp.mockClear();
  useApps.setState({ apps: [], loaded: false, probing: null });
});

afterEach(() => {
  cleanup();
  useApps.setState({ apps: [], loaded: false, probing: null });
});

test("uses MCP server wording in the add modal title", () => {
  render(<AddAppModal onClose={() => {}} />);

  expect(screen.getByText("Add MCP server")).toBeTruthy();
  expect(screen.queryByText("Add app")).toBeNull();

  cleanup();
});
