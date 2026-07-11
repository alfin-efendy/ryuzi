import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { ChoiceCard, RadioGroup } from "../../index";

afterEach(cleanup);

test("clicking anywhere on a Choice Card selects its radio value", () => {
  const onValueChange = mock((_value: string) => {});
  render(
    <RadioGroup aria-label="Source" value="folder" onValueChange={onValueChange}>
      <ChoiceCard value="folder" title="Open folder" description="Use an existing folder." />
      <ChoiceCard value="clone" title="Clone from URL" description="Clone a Git repository." />
    </RadioGroup>,
  );
  fireEvent.click(screen.getByText("Clone a Git repository."));
  expect(onValueChange.mock.calls[onValueChange.mock.calls.length - 1]?.[0]).toBe("clone");
});

test("arrow keys move selection using radio semantics", async () => {
  const onValueChange = mock((_value: string) => {});
  render(
    <RadioGroup aria-label="Source" value="folder" onValueChange={onValueChange}>
      <ChoiceCard value="folder" title="Open folder" />
      <ChoiceCard value="clone" title="Clone from URL" />
    </RadioGroup>,
  );
  const folder = screen.getByRole("radio", { name: /Open folder/ });
  folder.focus();
  fireEvent.keyDown(folder, { key: "ArrowRight" });
  await waitFor(() => expect(onValueChange.mock.calls[onValueChange.mock.calls.length - 1]?.[0]).toBe("clone"));
});

test("disabled card is exposed as disabled and cannot change selection", () => {
  const onValueChange = mock((_value: string) => {});
  render(
    <RadioGroup aria-label="Source" value="folder" onValueChange={onValueChange}>
      <ChoiceCard value="folder" title="Open folder" />
      <ChoiceCard value="clone" title="Clone from URL" disabled />
    </RadioGroup>,
  );
  const clone = screen.getByRole("radio", { name: /Clone from URL/ });
  expect(clone.getAttribute("aria-disabled") === "true" || (clone as HTMLButtonElement).disabled).toBe(true);
  fireEvent.click(screen.getByText("Clone from URL"));
  expect(onValueChange).not.toHaveBeenCalled();
});
