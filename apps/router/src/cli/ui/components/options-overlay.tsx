import React from "react";
import { Box, Text } from "ink";
import { theme } from "../theme";

const BINDINGS: Array<[string, string]> = [
  ["Tab / 1-4 / arrows", "switch tabs"],
  ["s", "start / stop daemon (Daemon tab)"],
  ["Enter", "open / edit"],
  ["Esc", "back / cancel"],
  ["?", "toggle this help"],
  ["q", "quit"],
];

export function OptionsOverlay() {
  return (
    <Box flexDirection="column" borderStyle="round" borderColor={theme.accent} paddingX={1}>
      <Text bold color={theme.accent}>Options</Text>
      {BINDINGS.map(([k, d]) => (
        <Text key={k}><Text color={theme.warn}>{k.padEnd(20)}</Text> {d}</Text>
      ))}
    </Box>
  );
}
