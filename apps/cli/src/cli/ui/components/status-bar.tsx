import React from "react";
import { Box, Text } from "ink";
import { palette } from "../theme";
import { KeyHint } from "./key-hint";

export type Hint = { k: string; label: string };

export function StatusBar({ hints }: { hints: Hint[] }) {
  return (
    <Box>
      {hints.map((h, i) => (
        <Box key={h.k}>
          {i > 0 && <Text color={palette.dim}>{"  ·  "}</Text>}
          <KeyHint k={h.k} label={h.label} />
        </Box>
      ))}
    </Box>
  );
}
