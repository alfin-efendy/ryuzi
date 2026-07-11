import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import type { ConnectionInfo } from "@/bindings";
import { RenameAccountModal } from "./RenameAccountModal";

const connection = {
  id: "account-1",
  provider: "openai-oauth",
  providerName: "ChatGPT",
  color: "#111",
  initial: "C",
  authType: "oauth",
  label: "Personal",
  priority: 0,
  enabled: true,
  quotaCapability: "codex",
  models: [],
  needsRelogin: false,
} satisfies ConnectionInfo;

afterEach(cleanup);

test("uses the shared modal shell and disables Save for empty or unchanged trimmed names", () => {
  render(<RenameAccountModal open connection={connection} onClose={() => {}} onRename={async () => true} />);
  const dialog = screen.getByRole("dialog", { name: "Rename account" });
  expect(dialog.querySelector('[data-slot="modal-header"]')).not.toBeNull();
  expect(dialog.querySelector('[data-slot="modal-footer"]')).not.toBeNull();
  expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
  const save = screen.getByRole("button", { name: "Save" }) as HTMLButtonElement;
  expect(save.disabled).toBe(true);
  fireEvent.change(screen.getByRole("textbox", { name: "Account name" }), { target: { value: "   " } });
  expect(save.disabled).toBe(true);
  fireEvent.change(screen.getByRole("textbox", { name: "Account name" }), { target: { value: " Personal " } });
  expect(save.disabled).toBe(true);
});

test("submits one trimmed name and closes only when rename succeeds", async () => {
  const onClose = mock(() => {});
  const onRename = mock(async (_name: string) => false);
  render(<RenameAccountModal open connection={connection} onClose={onClose} onRename={onRename} />);
  const input = screen.getByRole("textbox", { name: "Account name" });
  fireEvent.change(input, { target: { value: "  Work  " } });
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(onRename).toHaveBeenCalledWith("Work"));
  expect(onClose).not.toHaveBeenCalled();

  onRename.mockImplementationOnce(async () => true);
  fireEvent.click(screen.getByRole("button", { name: "Save" }));
  await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
});

test("focuses the name field and Cancel closes without renaming", async () => {
  const onClose = mock(() => {});
  const onRename = mock(async (_name: string) => true);
  render(<RenameAccountModal open connection={connection} onClose={onClose} onRename={onRename} />);
  const input = screen.getByRole("textbox", { name: "Account name" });
  await waitFor(() => expect(document.activeElement).toBe(input));
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(onRename).not.toHaveBeenCalled();
});
