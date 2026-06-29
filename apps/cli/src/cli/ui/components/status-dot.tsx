import React from "react";
import { Text } from "ink";
import { palette, symbols } from "../theme";

export function StatusDot({ on, label }: { on: boolean; label?: string }) {
  const s = symbols();
  return (
    <Text color={on ? palette.ok : palette.dim}>
      {on ? s.dot : s.dotOff}
      {label ? " " + label : ""}
    </Text>
  );
}
