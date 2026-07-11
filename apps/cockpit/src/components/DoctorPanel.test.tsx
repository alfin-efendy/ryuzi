import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen } from "@testing-library/react";
import type { DoctorFinding } from "@/bindings";

const pluginDoctor = mock((): Promise<{ status: "ok"; data: DoctorFinding[] }> => Promise.resolve({ status: "ok", data: [] }));
const toastError = mock((_message: string) => {});

mock.module("@/bindings", () => ({
  commands: { pluginDoctor },
}));
mock.module("sonner", () => ({
  toast: { error: toastError, success: mock(() => {}), info: mock(() => {}), warning: mock(() => {}) },
  Toaster: () => null,
}));

const { DoctorPanel } = await import("./DoctorPanel");
const { usePlugins } = await import("@/store-plugins");

const onClose = mock(() => {});

beforeEach(() => {
  pluginDoctor.mockClear();
  pluginDoctor.mockImplementation(() => Promise.resolve({ status: "ok", data: [] }));
  toastError.mockClear();
  onClose.mockClear();
  usePlugins.setState({ doctorFindings: [], doctorLoaded: false });
});

afterEach(() => {
  cleanup();
  usePlugins.setState({ doctorFindings: [], doctorLoaded: false });
});

async function renderPanel() {
  const result = render(<DoctorPanel onClose={onClose} />);
  await act(async () => {});
  return result;
}

test("fetches findings on mount and shows a clean message when there are none", async () => {
  await renderPanel();
  expect(pluginDoctor).toHaveBeenCalled();
  expect(screen.getByText("No issues found.")).toBeTruthy();
});

test("renders error findings above warn findings, each with message and suggested action", async () => {
  const findings: DoctorFinding[] = [
    {
      pluginId: "linear",
      severity: "warn",
      kind: "reconnect-required",
      message: "linear's sign-in expired",
      suggestedAction: "Reconnect linear",
    },
    {
      pluginId: "github",
      severity: "error",
      kind: "missing-binary",
      message: "github needs `npx`",
      suggestedAction: "Install npx or disable github",
    },
  ];
  pluginDoctor.mockImplementationOnce(() => Promise.resolve({ status: "ok", data: findings }));
  await renderPanel();

  expect(await screen.findByText("github needs `npx`")).toBeTruthy();
  expect(screen.getByText("Install npx or disable github")).toBeTruthy();
  expect(screen.getByText("linear's sign-in expired")).toBeTruthy();
  expect(screen.getByText("Reconnect linear")).toBeTruthy();

  const rows = screen.getAllByText(/github|linear/, { selector: "div.font-medium" });
  expect(rows.map((r) => r.textContent)).toEqual(["github", "linear"]);
});

test("toasts when the doctor check fails", async () => {
  pluginDoctor.mockImplementationOnce(() => Promise.resolve({ status: "error", error: { message: "boom" } } as never));
  await renderPanel();

  expect(toastError).toHaveBeenCalledWith("Doctor check failed: boom");
});

test("Close calls onClose", async () => {
  await renderPanel();
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  expect(onClose).toHaveBeenCalled();
});
