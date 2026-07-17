import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { AddCustomProviderModal } from "./AddCustomProviderModal";

afterEach(cleanup);

test("renders nothing while closed", () => {
  render(<AddCustomProviderModal open={false} onClose={() => {}} onCreate={() => Promise.resolve(true)} />);
  expect(screen.queryByText("Add custom provider")).toBeNull();
});

test("Create is disabled until a name is entered", () => {
  render(<AddCustomProviderModal open onClose={() => {}} onCreate={() => Promise.resolve(true)} />);
  const create = screen.getByRole("button", { name: "Create" }) as HTMLButtonElement;
  expect(create.disabled).toBe(true);
  fireEvent.change(screen.getByLabelText("Provider name"), { target: { value: "My Gateway" } });
  expect(create.disabled).toBe(false);
});

test("typing a name and clicking Create calls onCreate then closes", async () => {
  const onCreate = mock((_name: string) => Promise.resolve(true));
  const onClose = mock(() => {});
  render(<AddCustomProviderModal open onClose={onClose} onCreate={onCreate} />);
  fireEvent.change(screen.getByLabelText("Provider name"), { target: { value: "  My Gateway  " } });
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(onCreate).toHaveBeenCalledWith("My Gateway"));
  await waitFor(() => expect(onClose).toHaveBeenCalledTimes(1));
});

test("a failed create keeps the modal open", async () => {
  const onCreate = mock((_name: string) => Promise.resolve(false));
  const onClose = mock(() => {});
  render(<AddCustomProviderModal open onClose={onClose} onCreate={onCreate} />);
  fireEvent.change(screen.getByLabelText("Provider name"), { target: { value: "Gate" } });
  fireEvent.click(screen.getByRole("button", { name: "Create" }));
  await waitFor(() => expect(onCreate).toHaveBeenCalledWith("Gate"));
  expect(onClose).not.toHaveBeenCalled();
});

test("Cancel closes without creating", () => {
  const onCreate = mock((_name: string) => Promise.resolve(true));
  const onClose = mock(() => {});
  render(<AddCustomProviderModal open onClose={onClose} onCreate={onCreate} />);
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(onCreate).not.toHaveBeenCalled();
});
