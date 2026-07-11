import { afterEach, beforeEach, expect, mock, test } from "bun:test";
import { act, cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";

const addGateway = mock((_name: string, _host: string, _port: number, _username: string): Promise<boolean> => Promise.resolve(true));

const { useGateways } = await import("@/store-gateways");
const { AddGatewayModal } = await import("./AddGatewayModal");

beforeEach(() => {
  addGateway.mockClear();
  useGateways.setState({ gateways: [], eventsById: {}, activeGateway: "local", loaded: true, probing: false, add: addGateway });
});

afterEach(cleanup);

test("locks dismissal while probing and settles after the deferred save", async () => {
  let resolveAdd: ((ok: boolean) => void) | undefined;
  addGateway.mockImplementationOnce(
    () =>
      new Promise<boolean>((resolve) => {
        resolveAdd = resolve;
      }),
  );
  const onClose = mock(() => {});
  render(<AddGatewayModal onClose={onClose} />);

  fireEvent.change(screen.getByLabelText("Host"), { target: { value: "128.140.42.7" } });
  fireEvent.click(screen.getByRole("button", { name: "Connect" }));

  const dialog = screen.getByRole("dialog", { name: "Connect gateway" });
  const close = screen.getByRole("button", { name: "Close" }) as HTMLButtonElement;
  const cancel = screen.getByRole("button", { name: "Cancel" }) as HTMLButtonElement;
  const submit = screen.getByRole("button", { name: "Probing…" }) as HTMLButtonElement;
  await waitFor(() => expect(dialog.getAttribute("aria-busy")).toBe("true"));
  expect(close.disabled).toBe(true);
  expect(cancel.disabled).toBe(true);
  expect(submit.disabled).toBe(true);

  fireEvent.click(close);
  fireEvent.click(cancel);
  fireEvent.keyDown(document, { key: "Escape" });
  fireEvent.click(document.querySelector('[data-slot="modal-backdrop"]') as HTMLElement);
  expect(onClose).not.toHaveBeenCalled();
  expect(screen.getByRole("dialog", { name: "Connect gateway" })).toBeTruthy();

  await act(async () => resolveAdd?.(true));
  await waitFor(() => expect(close.disabled).toBe(false));
  expect(cancel.disabled).toBe(false);
  expect(onClose).toHaveBeenCalledTimes(1);
});
