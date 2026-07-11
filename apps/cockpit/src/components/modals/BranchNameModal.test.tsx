import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { BranchNameModal } from "./BranchNameModal";

afterEach(cleanup);

function setup() {
  const onClose = mock(() => {});
  const onCreate = mock((_: string) => {});
  render(<BranchNameModal open onClose={onClose} existingBranches={["main", "develop"]} onCreate={onCreate} />);
  return { onClose, onCreate };
}

const input = () => screen.getByPlaceholderText("feat/my-change");
const createButton = () => screen.getByRole("button", { name: "Create" }) as HTMLButtonElement;

test("renders nothing while closed", () => {
  render(<BranchNameModal open={false} onClose={() => {}} existingBranches={[]} onCreate={() => {}} />);
  expect(screen.queryByRole("dialog")).toBeNull();
});

test("Create is disabled until a valid name is typed", () => {
  setup();
  const dialog = screen.getByRole("dialog", { name: "New Branch" });
  expect(dialog.querySelector('[data-slot="modal-header"]')).not.toBeNull();
  expect(dialog.querySelector('[data-slot="modal-body"]')).not.toBeNull();
  expect(dialog.querySelector('[data-slot="modal-footer"]')).not.toBeNull();
  expect(screen.getByRole("button", { name: "Close" })).toBeTruthy();
  expect(createButton().disabled).toBe(true);
  fireEvent.change(input(), { target: { value: "feat/x" } });
  expect(createButton().disabled).toBe(false);
});

test("focus starts on the branch name input", async () => {
  setup();
  await waitFor(() => expect(document.activeElement).toBe(input()));
});

test("typed spaces normalize to dashes; existing names show an error and keep Create disabled", () => {
  setup();
  fireEvent.change(input(), { target: { value: "has space" } });
  expect((input() as HTMLInputElement).value).toBe("has-space");
  expect(createButton().disabled).toBe(false);
  fireEvent.change(input(), { target: { value: "main" } });
  expect(screen.getByText('Branch "main" already exists')).toBeTruthy();
  expect(createButton().disabled).toBe(true);
});

test("Create submits the normalized name and closes; no git command is involved", () => {
  const { onClose, onCreate } = setup();
  fireEvent.change(input(), { target: { value: "my new feature" } });
  fireEvent.click(createButton());
  expect(onCreate).toHaveBeenCalledWith("my-new-feature");
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("pasted surrounding whitespace normalizes to dashes in the input but submits the dash-stripped name", () => {
  const { onClose, onCreate } = setup();
  fireEvent.change(input(), { target: { value: "  feat/login " } });
  expect((input() as HTMLInputElement).value).toBe("-feat/login-");
  expect(createButton().disabled).toBe(false);
  fireEvent.click(createButton());
  expect(onCreate).toHaveBeenCalledWith("feat/login");
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("Enter submits a valid name; Enter on an invalid name does nothing", () => {
  const { onClose, onCreate } = setup();
  fireEvent.change(input(), { target: { value: "main" } });
  fireEvent.keyDown(input(), { key: "Enter" });
  expect(onCreate).not.toHaveBeenCalled();
  fireEvent.change(input(), { target: { value: "feat/ok" } });
  fireEvent.keyDown(input(), { key: "Enter" });
  expect(onCreate).toHaveBeenCalledWith("feat/ok");
  expect(onClose).toHaveBeenCalledTimes(1);
});

test("Cancel closes without creating", () => {
  const { onClose, onCreate } = setup();
  fireEvent.click(screen.getByRole("button", { name: "Cancel" }));
  expect(onClose).toHaveBeenCalledTimes(1);
  expect(onCreate).not.toHaveBeenCalled();
});
