import React from "react";
import { Text } from "ink";
import { theme } from "../theme";

export function StatusDot({ on, label }: { on: boolean; label?: string }) {
  return <Text color={on ? theme.ok : theme.dim}>{on ? "●" : "○"}{label ? " " + label : ""}</Text>;
}
