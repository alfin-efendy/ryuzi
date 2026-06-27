import React, { useState } from "react";
import { Box, Text, useInput } from "ink";
import type { AppController } from "../controller";
import { theme } from "../theme";

export function SessionsTab({ controller }: { controller: AppController }) {
  const sessions = controller.sessions();
  const [idx, setIdx] = useState(0);
  const [open, setOpen] = useState(false);

  useInput((_in, key) => {
    if (open) { if (key.escape) setOpen(false); return; }
    if (key.upArrow) setIdx((i) => (i > 0 ? i - 1 : Math.max(0, sessions.length - 1)));
    else if (key.downArrow) setIdx((i) => (i < sessions.length - 1 ? i + 1 : 0));
    else if (key.return && sessions.length) setOpen(true);
  });

  if (sessions.length === 0) {
    return <Text color={theme.dim}>no sessions yet — start the daemon and run from Discord</Text>;
  }
  const sel = sessions[Math.max(0, Math.min(idx, sessions.length - 1))];
  if (!sel) return <Text color={theme.dim}>no sessions yet — start the daemon and run from Discord</Text>;
  if (open) {
    return (
      <Box flexDirection="column">
        <Text bold>{sel.title ?? sel.sessionPk.slice(0, 8)}</Text>
        <Text color={theme.dim}>project {sel.projectId} · {sel.status} · by {sel.startedBy ?? "?"}</Text>
        <Box marginTop={1}><Text>{sel.lastText ?? "(no output captured)"}</Text></Box>
        <Text color={theme.dim}>Esc back</Text>
      </Box>
    );
  }
  return (
    <Box flexDirection="column">
      {sessions.map((s, i) => (
        <Text key={s.sessionPk} color={i === idx ? theme.accent : theme.text}>
          {(i === idx ? "› " : "  ")}{(s.title ?? s.sessionPk.slice(0, 8)).padEnd(28)} {s.status}
        </Text>
      ))}
      <Text color={theme.dim}>↑↓ select · Enter open</Text>
    </Box>
  );
}
