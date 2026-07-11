import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { useRef, useState } from "react";
import { Button, Combobox, Input, MenuPanel, MenuPanelItem, Modal, ModalBody, ModalFooter, ModalHeader } from "../../index";

afterEach(cleanup);

function DialogFixture({
  busy = false,
  initialOpen = true,
  explicitFinalFocus = false,
}: {
  busy?: boolean;
  initialOpen?: boolean;
  explicitFinalFocus?: boolean;
}) {
  const [open, setOpen] = useState(initialOpen);
  const firstInput = useRef<HTMLInputElement>(null);
  const returnTarget = useRef<HTMLButtonElement>(null);
  return (
    <>
      <Button onClick={() => setOpen(true)}>Open fixture</Button>
      <Button ref={returnTarget}>Explicit return</Button>
      {open && (
        <Modal
          onClose={() => setOpen(false)}
          width={420}
          busy={busy}
          initialFocus={firstInput}
          finalFocus={explicitFinalFocus ? returnTarget : undefined}
        >
          <ModalHeader title="Fixture title" description="Fixture description" />
          <ModalBody>
            <Input ref={firstInput} aria-label="Fixture input" />
          </ModalBody>
          <ModalFooter>
            <Button variant="outline" onClick={() => setOpen(false)} disabled={busy}>
              Cancel
            </Button>
          </ModalFooter>
        </Modal>
      )}
    </>
  );
}

test("dialog is labelled and the header X closes it", async () => {
  render(<DialogFixture />);
  expect(screen.getByRole("dialog", { name: "Fixture title", description: "Fixture description" })).toBeTruthy();
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("textbox", { name: "Fixture input" })));
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  await waitFor(() => expect(screen.queryByRole("dialog")).toBeNull());
});

test("focus starts at initialFocus and returns to the opener", async () => {
  render(<DialogFixture initialOpen={false} />);
  const opener = screen.getByRole("button", { name: "Open fixture" });
  opener.focus();
  fireEvent.click(opener);
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("textbox", { name: "Fixture input" })));
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  await waitFor(() => expect(document.activeElement).toBe(opener));
});

test("an explicit finalFocus target wins when opening was programmatic", async () => {
  render(<DialogFixture explicitFinalFocus />);
  fireEvent.click(screen.getByRole("button", { name: "Close" }));
  await waitFor(() => expect(document.activeElement).toBe(screen.getByRole("button", { name: "Explicit return" })));
});

test("busy dialog keeps X, Escape, and backdrop from closing", async () => {
  render(<DialogFixture busy />);
  expect((screen.getByRole("button", { name: "Close" }) as HTMLButtonElement).disabled).toBe(true);
  fireEvent.keyDown(document, { key: "Escape" });
  fireEvent.click(document.querySelector('[data-slot="modal-backdrop"]') as HTMLElement);
  await waitFor(() => expect(screen.getByRole("dialog", { name: "Fixture title" })).toBeTruthy());
});

test("Escape closes a nested Combobox before the dialog", async () => {
  const onClose = mock(() => {});
  render(
    <Modal onClose={onClose} width={420}>
      <ModalHeader title="Nested popup" />
      <ModalBody>
        <Combobox aria-label="Fruit" options={[{ value: "apple", label: "Apple" }]} value={null} onValueChange={() => {}} />
      </ModalBody>
      <ModalFooter />
    </Modal>,
  );
  fireEvent.click(screen.getByRole("combobox", { name: "Fruit" }));
  await screen.findByRole("listbox");
  fireEvent.keyDown(screen.getByRole("listbox"), { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByRole("dialog", { name: "Nested popup" })).toBeTruthy();
});

test("Escape closes a nested MenuPanel before the dialog", async () => {
  const onClose = mock(() => {});
  function Fixture() {
    const [menuOpen, setMenuOpen] = useState(true);
    return (
      <Modal onClose={onClose} width={420}>
        <ModalHeader title="Nested menu" />
        <ModalBody>
          {menuOpen && (
            <MenuPanel onClose={() => setMenuOpen(false)}>
              <MenuPanelItem>Choice</MenuPanelItem>
            </MenuPanel>
          )}
        </ModalBody>
        <ModalFooter />
      </Modal>
    );
  }
  render(<Fixture />);
  fireEvent.keyDown(screen.getByRole("button", { name: "Choice" }), { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("button", { name: "Choice" })).toBeNull());
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByRole("dialog", { name: "Nested menu" })).toBeTruthy();
});
