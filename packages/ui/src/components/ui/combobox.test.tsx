import { afterEach, expect, mock, test } from "bun:test";
import { cleanup, fireEvent, render, screen, waitFor } from "@testing-library/react";
import { Button, Combobox, type ComboboxGroup, type ComboboxOption } from "../../index";

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

test("groups render section labels and options stay selectable", async () => {
  const groups: ComboboxGroup[] = [
    {
      label: "Anthropic",
      options: [
        { value: "opus", label: "Opus" },
        { value: "sonnet", label: "Sonnet" },
      ],
    },
    { label: "Local", options: [{ value: "llama", label: "Llama" }] },
  ];
  const onChange = mock((_: string) => {});
  render(<Combobox options={groups} value={null} onValueChange={onChange} aria-label="Model" />);
  await openCombobox("Model");
  expect(screen.getByText("Anthropic")).toBeTruthy();
  expect(screen.getByText("Local")).toBeTruthy();
  expect(screen.getAllByRole("option").length).toBe(3);
  fireEvent.click(screen.getByRole("option", { name: /Sonnet/ }));
  expect(onChange).toHaveBeenCalledWith("sonnet");
});

test('allowCreate surfaces Create "<input>" and calls onCreate, not onValueChange', async () => {
  const onChange = mock((_: string) => {});
  const onCreate = mock((_: string) => {});
  render(<Combobox options={few} value={null} onValueChange={onChange} onCreate={onCreate} allowCreate aria-label="Branch" />);
  await openCombobox("Branch");
  // allowCreate forces the search input even below the threshold — creating requires typing.
  const input = screen.getByPlaceholderText("Search…");
  fireEvent.change(input, { target: { value: "feature/x" } });
  const createOption = await screen.findByRole("option", { name: /Create "feature\/x"/ });
  fireEvent.click(createOption);
  expect(onCreate).toHaveBeenCalledWith("feature/x");
  expect(onChange).not.toHaveBeenCalled();
});

test("allowCreate: no Create item when the text matches an existing option", async () => {
  render(<Combobox options={few} value={null} onValueChange={() => {}} onCreate={() => {}} allowCreate aria-label="Branch" />);
  await openCombobox("Branch");
  fireEvent.change(screen.getByPlaceholderText("Search…"), { target: { value: "Apple" } });
  await waitFor(() => expect(screen.getByRole("option", { name: /Apple/ })).toBeTruthy());
  expect(screen.queryByRole("option", { name: /Create/ })).toBeNull();
});

test("footer renders as a pinned action row and is clickable", async () => {
  const onOpenFolder = mock(() => {});
  render(
    <Combobox
      options={few}
      value={null}
      onValueChange={() => {}}
      aria-label="Project"
      footer={
        <button type="button" onClick={onOpenFolder}>
          Open folder
        </button>
      }
    />,
  );
  await openCombobox("Project");
  fireEvent.click(screen.getByRole("button", { name: "Open folder" }));
  expect(onOpenFolder).toHaveBeenCalledTimes(1);
});

test("custom trigger content replaces the default button contents", async () => {
  // A <span> is a valid element, so under the render-prop merge it BECOMES the
  // trigger itself (role="combobox", accessible name, and click handling land
  // on the span directly) rather than being nested inside a wrapper <button>.
  // The observable a11y contract — accessible name + click-to-open — still
  // holds; Base UI additionally logs a dev-only warning here because
  // `nativeButton` defaults to true while the merged element is a <span>, not
  // a real <button> (see combobox.test.tsx history / task report for detail).
  render(<Combobox options={few} value={null} onValueChange={() => {}} aria-label="Branch" trigger={<span>main</span>} />);
  const trigger = screen.getByRole("combobox", { name: "Branch" });
  expect(trigger.tagName).toBe("SPAN");
  expect(trigger.textContent).toContain("main");
  expect(trigger.querySelector('[data-slot="combobox-value"]')).toBeNull();
  fireEvent.click(trigger);
  await screen.findByRole("listbox");
});

test("a Fragment trigger renders inside the default button, not as the trigger element", async () => {
  // A Fragment passes React.isValidElement, but Base UI's cloneElement has no
  // single DOM node to merge role/aria/handlers onto — it must fall back to
  // rendering as children of the default trigger <button> instead.
  const { container } = render(
    <Combobox
      options={few}
      value={null}
      onValueChange={() => {}}
      aria-label="Branch"
      trigger={
        <>
          <span>frag-label</span>
          <span>-extra</span>
        </>
      }
    />,
  );
  expect(container.querySelectorAll("button").length).toBe(1);

  const trigger = screen.getByRole("combobox", { name: "Branch" });
  expect(trigger.tagName).toBe("BUTTON");
  expect(trigger.textContent).toContain("frag-label");
  fireEvent.click(trigger);
  await screen.findByRole("listbox");
});

test("an interactive element trigger becomes the trigger itself — no nested <button>", async () => {
  // Base UI's render-prop merge should make the caller's <Button> the
  // trigger element itself instead of nesting it inside
  // ComboboxPrimitive.Trigger's own <button> (invalid <button><button> DOM).
  const { container } = render(
    <Combobox
      options={few}
      value={null}
      onValueChange={() => {}}
      aria-label="Branch"
      trigger={<Button variant="outline">Branch chip</Button>}
    />,
  );
  expect(container.querySelectorAll("button").length).toBe(1);

  const trigger = screen.getByRole("combobox", { name: "Branch" });
  expect(trigger.tagName).toBe("BUTTON");
  expect(trigger.textContent).toContain("Branch chip");
  fireEvent.click(trigger);
  await screen.findByRole("listbox");
});

test("createHintLabel renders a pinned row that clears and focuses the search input", async () => {
  render(
    <Combobox
      options={few}
      value={null}
      onValueChange={() => {}}
      onCreate={() => {}}
      allowCreate
      createHintLabel="Create and checkout new branch…"
      aria-label="Branch"
    />,
  );
  await openCombobox("Branch");
  const input = screen.getByPlaceholderText("Search…");
  fireEvent.change(input, { target: { value: "left" } });
  const hint = screen.getByRole("button", { name: /Create and checkout new branch…/ });
  fireEvent.click(hint);
  await waitFor(() => {
    expect((input as HTMLInputElement).value).toBe("");
    expect(document.activeElement).toBe(input);
  });
});

test("onCreateHint: clicking the hint row closes the popup, calls the callback, and skips the clear+focus fallback", async () => {
  const onCreateHint = mock(() => {});
  render(
    <Combobox
      options={few}
      value={null}
      onValueChange={() => {}}
      onCreate={() => {}}
      allowCreate
      createHintLabel="New Branch"
      onCreateHint={onCreateHint}
      aria-label="Branch"
    />,
  );
  await openCombobox("Branch");
  const input = screen.getByPlaceholderText("Search…") as HTMLInputElement;
  fireEvent.change(input, { target: { value: "left" } });
  // Spy directly on the search input's focus() method. The fallback branch
  // (no onCreateHint, see the test above) calls `inputRef.current?.focus()`
  // synchronously in the same click handler, before the popup unmounts — so
  // a spy call here proves the handler fell through to that branch instead
  // of returning right after onCreateHint(). Note: document.activeElement /
  // the input's value can't be used for this — Base UI unmounts the popup
  // synchronously on close and resets its own inputValue to "" as part of
  // that (independent of this component's code), which masks both signals
  // regardless of whether the fallback ran.
  const focusSpy = mock(() => {});
  input.focus = focusSpy;
  fireEvent.click(screen.getByRole("button", { name: /New Branch/ }));
  expect(onCreateHint).toHaveBeenCalledTimes(1);
  expect(focusSpy).not.toHaveBeenCalled();
  await waitFor(() => expect(screen.queryByRole("listbox")).toBeNull());
  expect(focusSpy).not.toHaveBeenCalled();
});
