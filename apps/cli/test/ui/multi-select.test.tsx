import { test, expect } from "bun:test";
import React from "react";
import { render } from "ink-testing-library";
import { MultiSelectList } from "../../src/cli/ui/components/multi-select-list";

const flush = () => new Promise((r) => setTimeout(r, 20));

test("renders checkboxes + descriptions; space toggles current row", async () => {
  const sel = new Set<string>(["a"]);
  const toggled: string[] = [];
  const items = [
    { id: "a", label: "Alpha", description: "first" },
    { id: "b", label: "Beta", description: "second" },
  ];
  const { lastFrame, stdin } = render(
    <MultiSelectList items={items} selected={sel} onToggle={(id) => toggled.push(id)} renderRight={(id) => (id === "a" ? "ok" : "")} />,
  );
  await flush();
  const f = lastFrame()!;
  expect(f).toContain("[x] Alpha");
  expect(f).toContain("[ ] Beta");
  expect(f).toContain("first");
  stdin.write("\x1b[B");
  await flush(); // down arrow
  stdin.write(" ");
  await flush(); // toggle Beta
  expect(toggled).toEqual(["b"]);
});
