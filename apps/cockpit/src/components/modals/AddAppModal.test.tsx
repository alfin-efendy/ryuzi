import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { AddAppInput } from "@/bindings";

const addApp = mock((_input: AddAppInput): Promise<boolean> => Promise.resolve(true));

const { useApps } = await import("@/store-apps");
const { AddAppModal } = await import("./AddAppModal");

beforeEach(() => {
  addApp.mockClear();
  useApps.setState({ apps: [], loaded: false, probing: null, add: addApp });
});

afterEach(() => {
  cleanup();
  useApps.setState({ apps: [], loaded: false, probing: null });
});

test("uses MCP server wording in the add modal title", () => {
  render(<AddAppModal onClose={() => {}} />);

  const dialog = screen.getByRole("dialog", { name: "Add MCP server" });
  expect(dialog.querySelector('[data-slot="modal-header"]')).not.toBeNull();
  expect(dialog.querySelector('[data-slot="modal-body"]')).not.toBeNull();
  expect(dialog.querySelector('[data-slot="modal-footer"]')).not.toBeNull();
  expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
  expect(screen.getByText("Add MCP server")).toBeTruthy();
  expect(screen.queryByText("Add app")).toBeNull();

  cleanup();
});

test("locks dismissal while connecting and settles after the deferred save", async () => {
  let resolveAdd: ((ok: boolean) => void) | undefined;
  addApp.mockImplementationOnce(
    () =>
      new Promise<boolean>((resolve) => {
        resolveAdd = resolve;
      }),
  );
  const onClose = mock(() => {});
  render(<AddAppModal onClose={onClose} />);

  fireEvent.change(screen.getByLabelText("Name"), { target: { value: "GitHub" } });
  fireEvent.change(screen.getByLabelText("Command"), { target: { value: "mcp-server" } });
  fireEvent.click(screen.getByRole("button", { name: "Add & connect" }));

  const dialog = screen.getByRole("dialog", { name: "Add MCP server" });
  const close = screen.getByRole("button", { name: "Close" }) as HTMLButtonElement;
  const cancel = screen.getByRole("button", { name: "Cancel" }) as HTMLButtonElement;
  const submit = screen.getByRole("button", { name: "Connecting…" }) as HTMLButtonElement;
  await waitFor(() => expect(dialog.getAttribute("aria-busy")).toBe("true"));
  expect(close.disabled).toBe(true);
  expect(cancel.disabled).toBe(true);
  expect(submit.disabled).toBe(true);

  fireEvent.click(close);
  fireEvent.click(cancel);
  fireEvent.keyDown(document, { key: "Escape" });
  fireEvent.click(document.querySelector('[data-slot="modal-backdrop"]') as HTMLElement);
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByRole("dialog", { name: "Add MCP server" })).toBeTruthy();

  await act(async () => resolveAdd?.(true));
  await waitFor(() => expect(close.disabled).toBe(false));
  expect(cancel.disabled).toBe(false);
  expect(onClose).toHaveBeenCalledTimes(1);
});
