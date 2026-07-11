import { expect, test } from "bun:test";
import { useState } from "react";
import { fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ConfirmActionModal } from "./ConfirmActionModal";

test("uses the shared modal shell, footer actions, close button, and restores trigger focus", async () => {
  const trigger = document.createElement("button");
  document.body.append(trigger);
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
});
