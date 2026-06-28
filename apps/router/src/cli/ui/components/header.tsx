import React from "react";
import { Box, Text } from "ink";
import { palette, symbols } from "../theme";
import { brandName } from "../../brand";
import { TabBar } from "./tab-bar";

export function Header({ tabs, active }: { tabs: string[]; active: number }) {
  return (
    <Box flexDirection="column">
      <Box>
        <Text bold color={palette.signature}>{symbols().glyph}</Text>
        <Text bold> {brandName}</Text>
      </Box>
      <Box marginTop={1}>
        <TabBar tabs={tabs} active={active} />
      </Box>
    </Box>
  );
}
