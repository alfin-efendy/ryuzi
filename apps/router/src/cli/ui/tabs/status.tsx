import React, { useEffect, useState } from "react";
import { Box, Text } from "ink";
import type { AppController } from "../controller";
import { Panel } from "../components/panel";
import { StatusDot } from "../components/status-dot";
import { palette, symbols } from "../theme";
import type { ToolInfo } from "../../../harness/detect";

export function StatusTab({ controller }: { controller: AppController }) {
  const [env, setEnv] = useState<{ git: ToolInfo; claude: ToolInfo } | null>(null);
  useEffect(() => {
    let on = true;
    controller.checkEnv().then((e) => on && setEnv(e));
    return () => {
      on = false;
    };
  }, [controller]);

  const d = controller.daemon();
  const sessions = controller.sessions();
  const active = sessions.filter((s) => s.status === "running").length;
  const missing = controller.missingRequired();
  const s = symbols();

  return (
    <Box flexDirection="column">
      <Panel title="Services">
        <Box>
          <Text>{"Daemon   "}</Text>
          <StatusDot on={d.running} label={d.running ? "running" : "stopped"} />
          <Text>{"    Discord  "}</Text>
          <StatusDot on={d.running} label={d.running ? "connected" : "—"} />
        </Box>
      </Panel>
      <Panel title="Sessions">
        <Text>
          <Text color={palette.text}>{active}</Text>
          <Text color={palette.dim}> active / {sessions.length} total</Text>
        </Text>
      </Panel>
      <Panel title="Environment">
        <Text>
          {"git "}
          <Text color={env?.git.found ? palette.ok : palette.dim}>{env?.git.found ? s.ok : "…"}</Text>
          {"   claude "}
          <Text color={env?.claude.found ? palette.ok : palette.dim}>{env?.claude.found ? s.ok : "…"}</Text>
        </Text>
      </Panel>
      {missing.length > 0 && (
        <Panel title="Action needed" focus>
          <Text color={palette.warn}>
            {s.warn} missing settings: {missing.join(", ")} — open Config (4)
          </Text>
        </Panel>
      )}
    </Box>
  );
}
