import React, { useEffect, useState } from "react";
import { Box, Text } from "ink";
import type { AppController } from "../controller";
import { StatusDot } from "../components/status-dot";
import { theme } from "../theme";
import type { ToolInfo } from "../../../harness/detect";

export function StatusTab({ controller }: { controller: AppController }) {
  const [env, setEnv] = useState<{ git: ToolInfo; claude: ToolInfo } | null>(null);
  useEffect(() => { let on = true; controller.checkEnv().then((e) => { if (on) setEnv(e); }); return () => { on = false; }; }, [controller]);
  const d = controller.daemon();
  const sessions = controller.sessions();
  const active = sessions.filter((s) => s.status === "running").length;
  const missing = controller.missingRequired();
  return (
    <Box flexDirection="column">
      <Box><Text>{"Daemon   "}</Text><StatusDot on={d.running} label={d.running ? "running" : "stopped"} /></Box>
      <Box><Text>{"Discord  "}</Text><StatusDot on={d.running} label={d.running ? "connected" : "—"} /></Box>
      <Text>{"Sessions "}{active} active / {sessions.length} total</Text>
      <Text>{"Env      "}git {env?.git.found ? "✓" : "…"}  claude {env?.claude.found ? "✓" : "…"}</Text>
      {missing.length > 0 && (
        <Box marginTop={1}><Text color={theme.warn}>⚠ missing settings: {missing.join(", ")} — open Config (4)</Text></Box>
      )}
    </Box>
  );
}
