import React from "react";
import { Text } from "ink";
import { palette } from "../theme";
import { Panel } from "./panel";

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
    <>
      {/* title rendered manually as a child: Panel.title uppercases, which would break the "Options" test */}
      <Panel focus>
        <Text bold color={palette.signature}>
          Options
        </Text>
        {BINDINGS.map(([k, d]) => (
          <Text key={k}>
            <Text color={palette.signature}>{k.padEnd(20)}</Text> {d}
          </Text>
        ))}
      </Panel>
    </>
  );
}
