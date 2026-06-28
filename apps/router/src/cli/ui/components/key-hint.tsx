import React from "react";
import { Text } from "ink";
import { palette } from "../theme";

export function KeyHint({ k, label }: { k: string; label: string }) {
  return (
    <Text>
      <Text color={palette.signature} bold>
        {k}
      </Text>
      <Text color={palette.dim}> {label}</Text>
    </Text>
  );
}
