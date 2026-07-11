import { afterEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ConfirmAccountActionModal, type ConfirmAccountAction } from "./ConfirmAccountActionModal";

afterEach(cleanup);

function renderAction(action: ConfirmAccountAction) {
  const onClose = mock(() => {});
  render(<ConfirmAccountActionModal open action={action} onClose={onClose} />);
  return onClose;
}

const trigger = () => document.createElement("button");

test("delete is destructive, explains permanence, and initially focuses Cancel", async () => {
  renderAction({ kind: "delete", accountName: "Work", onConfirm: async () => true, trigger: trigger() });
  expect(screen.getByRole("dialog", { name: "Delete account?" }).textContent).toContain("cannot be undone");
  expect(screen.getByRole("button", { name: "Delete account" }).getAttribute("data-variant")).toBe("destructive");
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "Cancel" })));
});

test("reset credit is a normal confirmation and closes only on true", async () => {
  const onConfirm = mock(async () => false);
  const onClose = renderAction({ kind: "resetCredit", accountName: "Personal", onConfirm, trigger: trigger() });
  const confirm = screen.getByRole("button", { name: "Reset credit" });
  expect(confirm.getAttribute("data-variant")).not.toBe("destructive");
  fireEvent.click(confirm);
  await waitFor(() => expect(onConfirm).toHaveBeenCalledTimes(1));
  expect(onClose).not.toHaveBeenCalled();
});

test("busy confirmation disables X and Cancel until the action settles", async () => {
  let resolve: ((value: boolean) => void) | undefined;
  const onClose = renderAction({
    kind: "delete",
    accountName: "Work",
    onConfirm: () => new Promise((done) => (resolve = done)),
    trigger: trigger(),
  });
  fireEvent.click(screen.getByRole("button", { name: "Delete account" }));
  await waitFor(() => expect(screen.getByRole("dialog", { name: "Delete account?" }).getAttribute("aria-busy")).toBe("true"));
  expect((screen.getByRole("button", { name: "Close" }) as HTMLButtonElement).disabled).toBe(true);
  expect((screen.getByRole("button", { name: "Cancel" }) as HTMLButtonElement).disabled).toBe(true);
  await act(async () => resolve?.(true));
  await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
});
