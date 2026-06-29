import React from "react";
import { Box, Text } from "ink";
import { palette } from "../theme";

export function TabBar({ tabs, active }: { tabs: string[]; active: number }) {
  return (
    <Box>
      {tabs.map((t, i) => (
        <Box key={t} marginRight={2}>
          <Text color={i === active ? palette.signature : palette.dim} bold={i === active}>
            {i + 1} {t}
          </Text>
        </Box>
      ))}
    </Box>
  );
}
