import { afterEach, expect, test } from "bun:test";
import { cleanup, render, screen } from "@testing-library/react";
import { Modal } from "../../index";

afterEach(cleanup);

test("modal portals to document.body so the scrim escapes containing blocks", () => {
  render(
    <div data-testid="host">
      <Modal onClose={() => {}} width={300}>
        <div>content</div>
      </Modal>
    </div>,
  );
  const dialog = screen.getByRole("dialog");
  const scrim = dialog.parentElement;
  // Portaled: the scrim is a direct child of <body>, NOT of the host div.
  expect(scrim?.parentElement).toBe(document.body);
  expect(screen.getByTestId("host").contains(dialog)).toBe(false);
});
