import React, { useEffect, useReducer, useState } from "react";
import { Box, useApp, useInput } from "ink";
import type { AppController } from "./controller";
import { useController } from "./use-controller";
import { Header } from "./components/header";
import { StatusBar, type Hint } from "./components/status-bar";
import { Panel } from "./components/panel";
import { OptionsOverlay } from "./components/options-overlay";
import { StatusTab } from "./tabs/status";
import { DaemonTab } from "./tabs/daemon";
import { SessionsTab } from "./tabs/sessions";
import { ConfigTab } from "./tabs/config";
import { Wizard } from "./wizard";

const TABS = ["Status", "Daemon", "Sessions", "Config"] as const;

function hintsFor(active: number, daemonRunning: boolean): Hint[] {
  const base: Hint[] = [{ k: "Tab", label: "switch" }];
  const perTab: Hint[][] = [
    [],
    [{ k: "s", label: daemonRunning ? "stop" : "start" }],
    [{ k: "↑↓", label: "select" }, { k: "Enter", label: "open" }],
    [{ k: "↑↓", label: "select" }, { k: "Enter", label: "edit" }],
  ];
  return [...base, ...(perTab[active] ?? []), { k: "?", label: "options" }, { k: "q", label: "quit" }];
}

export function App({ controller }: { controller: AppController }) {
  useController(controller);
  const { exit } = useApp();
  const [mode, setMode] = useState<"wizard" | "dashboard">(controller.isConfigured() ? "dashboard" : "wizard");
  const [active, setActive] = useState(0);
  const [showOptions, setShowOptions] = useState(false);
  const [editing, setEditing] = useState(false);
  const [, tick] = useReducer((x: number) => x + 1, 0);

  useEffect(() => {
    const t = setInterval(tick, 1000);
    return () => clearInterval(t);
  }, []);

  useInput(
    (input, key) => {
      if (input === "q") return void exit();
      if (input === "?") return void setShowOptions((v) => !v);
      if (key.tab || key.rightArrow) return void setActive((a) => (a + 1) % TABS.length);
      if (key.leftArrow) return void setActive((a) => (a - 1 + TABS.length) % TABS.length);
      if (/^[1-4]$/.test(input)) return void setActive(Number(input) - 1);
      if (input === "s" && active === 1) return void controller.toggleDaemon();
    },
    { isActive: mode === "dashboard" && !editing },
  );

  if (mode === "wizard") {
    return <Wizard controller={controller} onDone={() => setMode("dashboard")} />;
  }

  return (
    <Box flexDirection="column" padding={1}>
      <Header tabs={[...TABS]} active={active} />
      <Box marginY={1} flexDirection="column">
        {active === 0 && <StatusTab controller={controller} />}
        {active === 1 && <DaemonTab controller={controller} />}
        {active === 2 && <SessionsTab controller={controller} />}
        {active === 3 && <ConfigTab controller={controller} setEditing={setEditing} />}
      </Box>
      {showOptions && (
        <Box marginBottom={1}>
          <OptionsOverlay />
        </Box>
      )}
      <StatusBar hints={hintsFor(active, controller.daemon().running)} />
    </Box>
  );
}
