import React from "react";
import { Box, Text } from "ink";
import type { AppController } from "../controller";
import { Panel } from "../components/panel";
import { StatusDot } from "../components/status-dot";
import { Badge } from "../components/badge";
import { palette } from "../theme";

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
      <Panel title="Daemon" focus>
        <Box>
          {d.starting ? (
            <Badge tone="warn">● connecting…</Badge>
          ) : (
            <StatusDot on={d.running} label={d.running ? "running" : "stopped"} />
          )}
          <Text color={palette.dim}>{"    uptime "}{uptime(d.startedAt)}</Text>
        </Box>
        {d.lastError && <Text color={palette.bad}>error {d.lastError}</Text>}
      </Panel>
      <Panel title="Logs">
        {logs.length === 0 ? <Text color={palette.dim}>(none)</Text> : logs.map((l, i) => <Text key={i}>{l}</Text>)}
      </Panel>
    </Box>
  );
}
