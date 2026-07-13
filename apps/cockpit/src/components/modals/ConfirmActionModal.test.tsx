import { afterEach, expect, test } from "bun:test";
import { useState } from "react";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ConfirmActionModal } from "./ConfirmActionModal";

afterEach(() => {
  cleanup();
});

test("uses the shared modal shell, footer actions, close button, and restores trigger focus", async () => {
  const trigger = document.createElement("button");
  document.body.append(trigger);
  try {
    trigger.focus();
    function Harness() {
      const [open, setOpen] = useState(true);
      return (
        <ConfirmActionModal
          open={open}
          title="Delete route?"
          description="This cannot be undone."
          confirmLabel="Delete"
          trigger={trigger}
          onClose={() => setOpen(false)}
          onConfirm={async () => true}
        />
      );
    }
    render(<Harness />);

    const dialog = screen.getByRole("dialog", { name: "Delete route?" });
    expect(dialog.querySelector('[data-slot="modal-footer"]')).toBeTruthy();
    fireEvent.click(screen.getByRole("button", { name: "Close" }));
    await waitFor(() => expect(document.activeElement).toBe(trigger));
  } finally {
    trigger.remove();
  }
});

test("keeps confirmation enabled by default and supports explicitly disabling it", () => {
  const common = {
    open: true,
    title: "Delete route?",
    description: "This cannot be undone.",
    confirmLabel: "Delete",
    trigger: null,
    onClose: () => {},
    onConfirm: async () => true,
  };
  const { rerender } = render(<ConfirmActionModal {...common} />);
  expect((screen.getByRole("button", { name: "Delete" }) as HTMLButtonElement).disabled).toBe(false);

  rerender(<ConfirmActionModal {...common} confirmDisabled />);
  expect((screen.getByRole("button", { name: "Delete" }) as HTMLButtonElement).disabled).toBe(true);
});
