import React from "react";
import { Box, Text } from "ink";
import { theme } from "../theme";

export function TabBar({ tabs, active }: { tabs: string[]; active: number }) {
  return (
    <Box>
      {tabs.map((t, i) => (
        <Box key={t} marginRight={2}>
          <Text color={i === active ? theme.accent : theme.dim} bold={i === active} underline={i === active}>
            {i + 1} {t}
          </Text>
        </Box>
      ))}
    </Box>
  );
}
