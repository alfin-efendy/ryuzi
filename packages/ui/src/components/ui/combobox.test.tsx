import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Combobox, type ComboboxOption } from "../../index";

// happy-dom lacks a couple of layout APIs Base UI touches when positioning
// and scrolling the popup — stub them before anything renders.
if (typeof Element.prototype.scrollIntoView !== "function") {
  Element.prototype.scrollIntoView = () => {};
}
if (typeof globalThis.ResizeObserver === "undefined") {
  class ResizeObserverStub {
    observe() {}
    unobserve() {}
    disconnect() {}
  }
  globalThis.ResizeObserver = ResizeObserverStub as unknown as typeof ResizeObserver;
}

const fruits: ComboboxOption[] = [
  { value: "apple", label: "Apple" },
  { value: "banana", label: "Banana" },
  { value: "cherry", label: "Cherry" },
  { value: "date", label: "Date" },
  { value: "elderberry", label: "Elderberry" },
  { value: "fig", label: "Fig" },
  { value: "grape", label: "Grape" },
  { value: "honeydew", label: "Honeydew" },
]; // 8 options > default searchThreshold 6 → search input rendered
const few = fruits.slice(0, 3); // 3 options ≤ 6 → plain listbox, no search input

afterEach(cleanup);

// The trigger always has role="combobox" (Base UI marks it so whenever the
// input lives inside the popup, which is this component's only layout).
// Query it BEFORE opening so the role query cannot also match the popup input.
async function openCombobox(name: string) {
  const trigger = screen.getByRole("combobox", { name });
  fireEvent.click(trigger);
  await screen.findByRole("listbox");
  return trigger;
}

test("renders all options in the popup list", async () => {
  render(<Combobox options={fruits} value={null} onValueChange={() => {}} aria-label="Fruit" />);
  await openCombobox("Fruit");
  expect(screen.getAllByRole("option").length).toBe(8);
  expect(screen.getByRole("option", { name: /Apple/ })).toBeTruthy();
  expect(screen.getByRole("option", { name: /Honeydew/ })).toBeTruthy();
});

test("clicking an option calls onValueChange with the option value", async () => {
  const onChange = mock((_: string) => {});
  render(<Combobox options={fruits} value={null} onValueChange={onChange} aria-label="Fruit" />);
  await openCombobox("Fruit");
  fireEvent.click(screen.getByRole("option", { name: /Banana/ }));
  expect(onChange).toHaveBeenCalledWith("banana");
});

test("no search input at or below searchThreshold", async () => {
  render(<Combobox options={few} value={null} onValueChange={() => {}} aria-label="Fruit" />);
  await openCombobox("Fruit");
  expect(screen.queryByPlaceholderText("Search…")).toBeNull();
});

test("custom searchThreshold: search input shows when option count exceeds it", async () => {
  render(<Combobox options={few} searchThreshold={2} value={null} onValueChange={() => {}} aria-label="Fruit" />);
  await openCombobox("Fruit");
  expect(screen.getByPlaceholderText("Search…")).toBeTruthy();
});

test("typing filters the list", async () => {
  render(<Combobox options={fruits} value={null} onValueChange={() => {}} aria-label="Fruit" />);
  await openCombobox("Fruit");
  fireEvent.change(screen.getByPlaceholderText("Search…"), { target: { value: "ban" } });
  await waitFor(() => {
    expect(screen.getByRole("option", { name: /Banana/ })).toBeTruthy();
    expect(screen.queryByRole("option", { name: /Apple/ })).toBeNull();
  });
});

test("keyboard: arrows highlight, Enter selects (search input mode)", async () => {
  const onChange = mock((_: string) => {});
  render(<Combobox options={fruits} value={null} onValueChange={onChange} aria-label="Fruit" />);
  await openCombobox("Fruit");
  const input = screen.getByPlaceholderText("Search…");
  fireEvent.keyDown(input, { key: "ArrowDown" });
  fireEvent.keyDown(input, { key: "ArrowDown" });
  fireEvent.keyDown(input, { key: "Enter" });
  await waitFor(() => expect(onChange).toHaveBeenCalledWith("banana"));
});

test("keyboard: arrows + Enter work without a search input (small list)", async () => {
  const onChange = mock((_: string) => {});
  render(<Combobox options={few} value={null} onValueChange={onChange} aria-label="Fruit" />);
  await openCombobox("Fruit");
  const list = screen.getByRole("listbox");
  fireEvent.keyDown(list, { key: "ArrowDown" });
  fireEvent.keyDown(list, { key: "ArrowDown" });
  fireEvent.keyDown(list, { key: "Enter" });
  await waitFor(() => expect(onChange).toHaveBeenCalledWith("banana"));
});

test("Escape closes the popup", async () => {
  render(<Combobox options={fruits} value={null} onValueChange={() => {}} aria-label="Fruit" />);
  await openCombobox("Fruit");
  fireEvent.keyDown(screen.getByPlaceholderText("Search…"), { key: "Escape" });
  await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
});

test("selected option shows the Check indicator", async () => {
  render(<Combobox options={fruits} value="banana" onValueChange={() => {}} aria-label="Fruit" />);
  await openCombobox("Fruit");
  const banana = screen.getByRole("option", { name: /Banana/ });
  expect(banana.getAttribute("aria-selected")).toBe("true");
  expect(banana.querySelector('[data-slot="combobox-item-indicator"]')).not.toBeNull();
  const apple = screen.getByRole("option", { name: /Apple/ });
  expect(apple.querySelector('[data-slot="combobox-item-indicator"]')).toBeNull();
});

test("selected label shows on the trigger; aria-label lands on trigger and input", async () => {
  render(<Combobox options={fruits} value="cherry" onValueChange={() => {}} aria-label="Fruit" />);
  const trigger = screen.getByRole("combobox", { name: "Fruit" });
  expect(trigger.textContent).toContain("Cherry");
  await openCombobox("Fruit");
  expect(screen.getByPlaceholderText("Search…").getAttribute("aria-label")).toBe("Fruit");
});
