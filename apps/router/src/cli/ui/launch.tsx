import React from "react";
import { render } from "ink";
import { App } from "./app";
import { AppController } from "./controller";
import type { CliDeps } from "../run";
import { DiscordClientPort } from "../../gateways/discord/client-port";

export async function launchUi(deps: CliDeps): Promise<number> {
  const controller = new AppController({
    dbPath: deps.dbPath,
    detect: deps.detect,
    portFactory: (cfg) => new DiscordClientPort(cfg),
    harnessFactory: deps.harnessFactory,
  });
  const { waitUntilExit } = render(<App controller={controller} />, { alternateScreen: true, exitOnCtrlC: true });
  await waitUntilExit();
  controller.stopDaemon();
  return 0;
}
