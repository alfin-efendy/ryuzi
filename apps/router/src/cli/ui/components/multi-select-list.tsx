import React, { useState } from "react";
import { Box, Text, useInput } from "ink";
import { palette, symbols } from "../theme";

export interface MultiSelectItem {
  id: string;
  label: string;
  description?: string;
}

export function MultiSelectList({
  items, selected, onToggle, renderRight,
}: {
  items: MultiSelectItem[];
  selected: Set<string>;
  onToggle: (id: string) => void;
  renderRight?: (id: string) => string;
}) {
  const [idx, setIdx] = useState(0);
  const s = symbols();
  useInput((input, key) => {
    if (key.upArrow) setIdx((i) => (i > 0 ? i - 1 : items.length - 1));
    else if (key.downArrow) setIdx((i) => (i < items.length - 1 ? i + 1 : 0));
    else if (input === " " && items[idx]) onToggle(items[idx]!.id);
  });
  return (
    <Box flexDirection="column">
      {items.map((it, i) => {
        const right = renderRight?.(it.id) ?? "";
        const cur = i === idx;
        return (
          <Text key={it.id} color={cur ? palette.signature : palette.text}>
            {cur ? s.caret + " " : "  "}[{selected.has(it.id) ? "x" : " "}] {it.label}
            {it.description ? ` — ${it.description}` : ""}
            {right ? `  ${right}` : ""}
          </Text>
        );
      })}
    </Box>
  );
}
