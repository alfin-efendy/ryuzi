import React, { useState } from "react";
import { Box, Text, useInput } from "ink";
import type { AppController } from "../controller";
import { Panel } from "../components/panel";
import { Badge } from "../components/badge";
import { palette, symbols } from "../theme";

export function SessionsTab({ controller }: { controller: AppController }) {
  const sessions = controller.sessions();
  const [idx, setIdx] = useState(0);
  const [open, setOpen] = useState(false);
  const s = symbols();

  useInput((_in, key) => {
    if (open) {
      if (key.escape) setOpen(false);
      return;
    }
    if (key.upArrow) setIdx((i) => (i > 0 ? i - 1 : Math.max(0, sessions.length - 1)));
    else if (key.downArrow) setIdx((i) => (i < sessions.length - 1 ? i + 1 : 0));
    else if (key.return && sessions.length) setOpen(true);
  });

  if (sessions.length === 0) {
    return (
      <Panel title="Sessions">
        <Text color={palette.dim}>no sessions yet — start the daemon and run from Discord</Text>
      </Panel>
    );
  }
  const sel = sessions[Math.max(0, Math.min(idx, sessions.length - 1))];
  if (!sel) {
    return (
      <Panel title="Sessions">
        <Text color={palette.dim}>no sessions yet — start the daemon and run from Discord</Text>
      </Panel>
    );
  }
  if (open) {
    return (
      <Panel title={sel.title ?? sel.sessionPk.slice(0, 8)} focus>
        <Text color={palette.dim}>
          project {sel.projectId} · {sel.status} · by {sel.startedBy ?? "?"}
        </Text>
        <Box marginTop={1}>
          <Text>{sel.lastText ?? "(no output captured)"}</Text>
        </Box>
      </Panel>
    );
  }
  return (
    <Panel title="Sessions" focus>
      {sessions.map((row, i) => {
        const selected = i === idx;
        return (
          <Box key={row.sessionPk}>
            <Text color={palette.signature}>{selected ? s.marker + " " : "  "}</Text>
            <Text color={selected ? palette.text : palette.dim}>{(row.title ?? row.sessionPk.slice(0, 8)).padEnd(28)}</Text>
            <Badge tone={row.status === "running" ? "ok" : "dim"}> {row.status}</Badge>
          </Box>
        );
      })}
    </Panel>
  );
}
