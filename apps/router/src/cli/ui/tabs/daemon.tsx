import React from "react";
import { Box, Text } from "ink";
import type { AppController } from "../controller";
import { StatusDot } from "../components/status-dot";
import { theme } from "../theme";

function uptime(startedAt?: number): string {
  if (!startedAt) return "—";
  const s = Math.floor((Date.now() - startedAt) / 1000);
  const p = (n: number) => String(n).padStart(2, "0");
  return `${p(Math.floor(s / 3600))}:${p(Math.floor((s % 3600) / 60))}:${p(s % 60)}`;
}

export function DaemonTab({ controller }: { controller: AppController }) {
  const d = controller.daemon();
  const logs = controller.logs().slice(-8);
  return (
    <Box flexDirection="column">
      <Box>
        {d.starting
          ? <Text color={theme.warn}>● connecting…</Text>
          : <StatusDot on={d.running} label={d.running ? "running" : "stopped"} />}
      </Box>
      <Text>uptime  {uptime(d.startedAt)}</Text>
      {d.lastError && <Text color={theme.bad}>error   {d.lastError}</Text>}
      <Text color={theme.dim}>{d.starting ? "connecting to Discord…" : `press s to ${d.running ? "stop" : "start"}`}</Text>
      <Box flexDirection="column" marginTop={1} borderStyle="round" borderColor={theme.dim} paddingX={1}>
        <Text color={theme.dim}>logs</Text>
        {logs.length === 0 ? <Text color={theme.dim}>(none)</Text> : logs.map((l, i) => <Text key={i}>{l}</Text>)}
      </Box>
    </Box>
  );
}
