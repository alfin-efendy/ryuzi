import React from "react";
import { render } from "ink";
import { App } from "./app";
import { AppController } from "./controller";
import type { CliDeps } from "../run";

export async function launchUi(deps: CliDeps): Promise<number> {
  const controller = new AppController({ dbPath: deps.dbPath, detect: deps.detect });
  const { waitUntilExit } = render(<App controller={controller} />, { alternateScreen: true, exitOnCtrlC: true });
  await waitUntilExit();
  return 0;
}
