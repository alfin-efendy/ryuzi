import type React from "react";
import { Box, Text } from "ink";
import { palette, borderStyle } from "../theme";

export function Panel({ title, focus, children }: { title?: string; focus?: boolean; children: React.ReactNode }) {
  return (
    <Box flexDirection="column" borderStyle={borderStyle()} borderColor={focus ? palette.signature : palette.border} paddingX={1}>
      {title && <Text color={focus ? palette.signature : palette.dim}>{title.toUpperCase()}</Text>}
      {children}
    </Box>
  );
}
