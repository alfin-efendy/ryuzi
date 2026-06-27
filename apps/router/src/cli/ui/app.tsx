import React, { useEffect, useReducer, useState } from "react";
import { Box, Text, useApp, useInput } from "ink";
import type { AppController } from "./controller";
import { useController } from "./use-controller";
import { TabBar } from "./components/tab-bar";
import { OptionsOverlay } from "./components/options-overlay";
import { StatusTab } from "./tabs/status";
import { DaemonTab } from "./tabs/daemon";
import { SessionsTab } from "./tabs/sessions";
import { ConfigTab } from "./tabs/config";
import { Wizard } from "./wizard";
import { theme } from "./theme";
import { brandGlyph, brandName } from "../brand";

const TABS = ["Status", "Daemon", "Sessions", "Config"] as const;

export function App({ controller }: { controller: AppController }) {
  useController(controller);
  const { exit } = useApp();
  const [mode, setMode] = useState<"wizard" | "dashboard">(controller.isConfigured() ? "dashboard" : "wizard");
  const [active, setActive] = useState(0);
  const [showOptions, setShowOptions] = useState(false);
  const [editing, setEditing] = useState(false);
  const [, tick] = useReducer((x: number) => x + 1, 0);

  useEffect(() => { const t = setInterval(tick, 1000); return () => clearInterval(t); }, []);

  useInput((input, key) => {
    if (input === "q") { exit(); return; }
    if (input === "?") { setShowOptions((v) => !v); return; }
    if (key.tab || key.rightArrow) { setActive((a) => (a + 1) % TABS.length); return; }
    if (key.leftArrow) { setActive((a) => (a - 1 + TABS.length) % TABS.length); return; }
    if (/^[1-4]$/.test(input)) { setActive(Number(input) - 1); return; }
    if (input === "s" && active === 1) { void controller.toggleDaemon(); return; }
  }, { isActive: mode === "dashboard" && !editing });

  if (mode === "wizard") {
    return <Wizard controller={controller} onDone={() => setMode("dashboard")} />;
  }

  return (
    <Box flexDirection="column" padding={1}>
      <Box>
        <Text bold color={theme.accent}>{brandGlyph}</Text>
        <Text bold> {brandName}</Text>
        <Text color={theme.dim}> hr</Text>
      </Box>
      <Box marginY={1}><TabBar tabs={[...TABS]} active={active} /></Box>
      {active === 0 && <StatusTab controller={controller} />}
      {active === 1 && <DaemonTab controller={controller} />}
      {active === 2 && <SessionsTab controller={controller} />}
      {active === 3 && <ConfigTab controller={controller} setEditing={setEditing} />}
      {showOptions && <Box marginTop={1}><OptionsOverlay /></Box>}
      <Box marginTop={1}><Text color={theme.dim}>Tab switch · ? options · q quit</Text></Box>
    </Box>
  );
}
