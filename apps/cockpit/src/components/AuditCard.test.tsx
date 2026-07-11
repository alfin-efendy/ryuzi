import { afterEach, expect, mock, test } from "bun:test";
import { act, cleanup, render, screen } from "@testing-library/react";
import type { AuditRow, CmdError, Result } from "@/bindings";

// Mock the Tauri boundary before the component loads.
let seededRows: AuditRow[] = [];
const ok = <T,>(data: T): Result<T, CmdError> => ({ status: "ok", data });

const listAudit = mock(async () => ok(seededRows));

mock.module("@/bindings", () => ({
  commands: { listAudit },
}));

const { AuditCard } = await import("./AuditCard");

function seed(rows: AuditRow[]) {
  seededRows = rows;
}

afterEach(() => {
  cleanup();
  listAudit.mockClear();
});

test("shows the empty state when there is no audit activity", async () => {
  seed([]);
  await act(async () => {
    render(<AuditCard />);
  });

  expect(screen.getByText("No app-control activity yet.")).toBeTruthy();
});

test("renders recent audit rows", async () => {
  seed([{ id: 1, tool: "app_jobs", action: "create", decision: "allow", origin: "agent", sessionPk: "s", at: Date.now() }]);

  await act(async () => {
    render(<AuditCard />);
  });

  expect(listAudit).toHaveBeenCalledWith(100);
  expect(screen.getByText(/app_jobs/)).toBeTruthy();
  expect(screen.getByText(/create/)).toBeTruthy();
});
