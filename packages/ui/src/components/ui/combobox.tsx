import * as React from "react";
import { Combobox as ComboboxPrimitive } from "@base-ui/react/combobox";
import { Check, ChevronsUpDown, Plus } from "lucide-react";

import { cn } from "../../lib/utils";
import { buttonVariants } from "./button";

type ComboboxOption = {
  value: string;
  label: string;
  description?: string;
  mono?: boolean;
};

type ComboboxGroup = {
  label: string;
  options: ComboboxOption[];
};

type ComboboxProps = {
  options: ComboboxOption[] | ComboboxGroup[];
  value: string | null;
  onValueChange: (value: string) => void;
  placeholder?: string;
  disabled?: boolean;
  "aria-label"?: string;
  /** Offer a 'Create "<input>"' item when the typed text matches no option. */
  allowCreate?: boolean;
  onCreate?: (input: string) => void;
  /**
   * Search input is rendered only when the total option count exceeds this
   * (default 6). allowCreate always renders it — creating requires typing.
   */
  searchThreshold?: number;
  /** Pinned non-option action row below the list. */
  footer?: React.ReactNode;
  /**
   * Custom trigger content. When a React element is passed (e.g. a
   * `<Button>`), it BECOMES the trigger element itself: Base UI's `render`
   * prop clones it, merging on the trigger's role, aria attributes,
   * className, and event handlers — so passing an interactive element is
   * safe and never produces nested `<button>`s. Non-element content
   * (string, fragment, etc.) renders as children of the default trigger
   * `<button>` instead — a Fragment in particular has no single element to
   * merge onto, so it always renders as children. Default (no `trigger`) is
   * an outline button showing the selected label + ChevronsUpDown.
   */
  trigger?: React.ReactNode;
  className?: string;
  /** Pinned "+ <label>" row below the list that clears + focuses the search
   *  input — an affordance for allowCreate (typing a new name creates it). */
  createHintLabel?: string;
  /** When set, clicking the create-hint row closes the popup and calls this
   *  instead of the clear-and-focus default (e.g. to open a naming dialog). */
  onCreateHint?: () => void;
};

// Internal item shape handed to Base UI. `createInput` marks the synthetic
// 'Create "<input>"' item in allowCreate mode.
type ComboboxItemData = ComboboxOption & { createInput?: string };
// Internal group shape handed to Base UI (its grouped-items contract is `{ items }`).
type ComboboxGroupData = { label: string; items: ComboboxItemData[] };

function isGrouped(options: ComboboxOption[] | ComboboxGroup[]): options is ComboboxGroup[] {
  return options.length > 0 && "options" in options[0];
}

function ComboboxItemView({ item }: { item: ComboboxItemData }) {
  return (
    <ComboboxPrimitive.Item
      value={item}
      data-slot="combobox-item"
      className={cn(
        "grid cursor-default grid-cols-[minmax(0,1fr)_1rem] items-center gap-2 rounded-lg px-2.5 py-1.5 text-sm outline-none select-none",
        "data-highlighted:bg-accent data-highlighted:text-accent-foreground",
      )}
    >
      <span className="col-start-1 min-w-0">
        {item.createInput !== undefined ? (
          <span className="flex items-center gap-1.5">
            <Plus aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
            <span className="truncate">{item.label}</span>
          </span>
        ) : (
          <span className={cn("block truncate", item.mono && "font-mono text-[12.5px]")}>{item.label}</span>
        )}
        {item.description !== undefined && <span className="mt-0.5 block truncate text-xs text-muted-foreground">{item.description}</span>}
      </span>
      <ComboboxPrimitive.ItemIndicator data-slot="combobox-item-indicator" className="col-start-2">
        <Check aria-hidden size={14} strokeWidth={2.5} />
      </ComboboxPrimitive.ItemIndicator>
    </ComboboxPrimitive.Item>
  );
}

function Combobox({
  options,
  value,
  onValueChange,
  placeholder = "Select…",
  disabled = false,
  "aria-label": ariaLabel,
  allowCreate = false,
  onCreate,
  searchThreshold = 6,
  footer,
  trigger,
  className,
  createHintLabel,
  onCreateHint,
}: ComboboxProps) {
  const [query, setQuery] = React.useState("");
  // Controlled open state so the create-hint row can close the popup itself.
  const [open, setOpen] = React.useState(false);
  const inputRef = React.useRef<HTMLInputElement>(null);

  const flat = React.useMemo<ComboboxOption[]>(() => (isGrouped(options) ? options.flatMap((g) => g.options) : options), [options]);
  // allowCreate forces the input: creating requires typing.
  const showSearch = allowCreate || flat.length > searchThreshold;

  const trimmed = query.trim();
  const createItem = React.useMemo<ComboboxItemData | null>(() => {
    if (!allowCreate || trimmed === "") return null;
    const lowered = trimmed.toLowerCase();
    const exists = flat.some((o) => o.label.trim().toLowerCase() === lowered || o.value.toLowerCase() === lowered);
    // The label contains the typed text, so Base UI's contains-filter keeps it visible.
    return exists ? null : { value: `__create__:${trimmed}`, label: `Create "${trimmed}"`, createInput: trimmed };
  }, [allowCreate, trimmed, flat]);

  const items = React.useMemo<ComboboxItemData[] | ComboboxGroupData[]>(() => {
    if (isGrouped(options)) {
      const groups: ComboboxGroupData[] = options.map((g) => ({ label: g.label, items: g.options }));
      return createItem ? [...groups, { label: "", items: [createItem] }] : groups;
    }
    return createItem ? [...options, createItem] : options;
  }, [options, createItem]);

  const selected = React.useMemo<ComboboxItemData | null>(() => flat.find((o) => o.value === value) ?? null, [flat, value]);

  // An element trigger (e.g. <Button>) becomes the trigger element itself via
  // Base UI's `render` prop — no nested <button>. Non-element content (string,
  // fragment, etc.) has no element to merge onto, so it renders as children.
  // A Fragment IS a valid element per React.isValidElement, but Base UI's
  // cloneElement can't merge props onto it (no single DOM node to attach
  // role/aria/handlers to) — treat it like non-element content.
  const triggerElement = trigger !== undefined && React.isValidElement(trigger) && trigger.type !== React.Fragment ? trigger : undefined;

  return (
    <ComboboxPrimitive.Root<ComboboxItemData>
      items={items}
      value={selected}
      isItemEqualToValue={(a, b) => a?.value === b?.value}
      onValueChange={(next) => {
        if (!next) return;
        if (next.createInput !== undefined) onCreate?.(next.createInput);
        else onValueChange(next.value);
      }}
      inputValue={query}
      onInputValueChange={setQuery}
      open={open}
      onOpenChange={(nextOpen) => {
        setOpen(nextOpen);
        if (!nextOpen) setQuery("");
      }}
      disabled={disabled}
    >
      <ComboboxPrimitive.Trigger
        data-slot="combobox-trigger"
        aria-label={ariaLabel}
        className={cn(
          trigger !== undefined
            ? "cursor-pointer rounded-lg text-left outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
            : cn(buttonVariants({ variant: "outline" }), "justify-between gap-2 font-normal data-placeholder:text-muted-foreground"),
          className,
        )}
        render={triggerElement}
      >
        {triggerElement === undefined &&
          (trigger !== undefined ? (
            trigger
          ) : (
            <>
              <span data-slot="combobox-value" className="truncate">
                <ComboboxPrimitive.Value placeholder={placeholder} />
              </span>
              <ChevronsUpDown aria-hidden className="size-3.5 shrink-0 text-muted-foreground" />
            </>
          ))}
      </ComboboxPrimitive.Trigger>
      <ComboboxPrimitive.Portal>
        <ComboboxPrimitive.Positioner align="start" sideOffset={6} className="z-50 outline-none">
          <ComboboxPrimitive.Popup
            data-slot="combobox-popup"
            className={cn(
              "w-[max(var(--anchor-width),11rem)] max-w-[var(--available-width)] origin-(--transform-origin) rounded-xl border border-border surface-acrylic text-popover-foreground shadow-lg outline-none",
              "data-open:animate-in data-open:fade-in-0 data-open:zoom-in-95 data-closed:animate-out data-closed:fade-out-0 data-closed:zoom-out-95",
            )}
          >
            {showSearch && (
              <div data-slot="combobox-search" className="border-b border-border p-1.5">
                <ComboboxPrimitive.Input
                  ref={inputRef}
                  data-slot="combobox-input"
                  aria-label={ariaLabel}
                  placeholder="Search…"
                  className="h-7 w-full rounded-md bg-transparent px-1.5 text-sm outline-none placeholder:text-muted-foreground"
                />
              </div>
            )}
            <ComboboxPrimitive.Empty data-slot="combobox-empty">
              <div className="px-2.5 py-2 text-[13px] text-muted-foreground">No matches.</div>
            </ComboboxPrimitive.Empty>
            <ComboboxPrimitive.List
              data-slot="combobox-list"
              className="max-h-[min(60vh,380px)] overflow-y-auto overscroll-contain p-1.5 empty:p-0"
            >
              {(entry: ComboboxItemData | ComboboxGroupData) =>
                "items" in entry ? (
                  <ComboboxPrimitive.Group key={entry.label} items={entry.items} className="pb-1 last:pb-0">
                    {/* Empty label = headingless group (used for the synthetic Create item). */}
                    {entry.label !== "" && (
                      <ComboboxPrimitive.GroupLabel
                        data-slot="combobox-group-label"
                        className="px-2.5 pb-[5px] pt-[7px] text-[11px] font-semibold uppercase tracking-[0.04em] text-muted-foreground"
                      >
                        {entry.label}
                      </ComboboxPrimitive.GroupLabel>
                    )}
                    <ComboboxPrimitive.Collection>
                      {(item: ComboboxItemData) => <ComboboxItemView key={item.value} item={item} />}
                    </ComboboxPrimitive.Collection>
                  </ComboboxPrimitive.Group>
                ) : (
                  <ComboboxItemView key={entry.value} item={entry} />
                )
              }
            </ComboboxPrimitive.List>
            {createHintLabel !== undefined && (
              <div data-slot="combobox-create-hint" className="border-t border-border p-1.5">
                <button
                  type="button"
                  onClick={() => {
                    if (onCreateHint) {
                      setOpen(false);
                      onCreateHint();
                      return;
                    }
                    setQuery("");
                    inputRef.current?.focus();
                  }}
                  className="flex w-full cursor-pointer items-center gap-1.5 rounded-lg px-2.5 py-1.5 text-sm text-muted-foreground outline-none hover:bg-accent hover:text-accent-foreground"
                >
                  <Plus aria-hidden className="size-3.5 shrink-0" />
                  {createHintLabel}
                </button>
              </div>
            )}
            {footer !== undefined && (
              <div data-slot="combobox-footer" className="border-t border-border p-1.5">
                {footer}
              </div>
            )}
          </ComboboxPrimitive.Popup>
        </ComboboxPrimitive.Positioner>
      </ComboboxPrimitive.Portal>
    </ComboboxPrimitive.Root>
  );
}

export { Combobox };
export type { ComboboxOption, ComboboxGroup, ComboboxProps };
