// apps/ide/src/preload/index.ts — surface filled in Task 4.
import { contextBridge } from "electron";

contextBridge.exposeInMainWorld("harness", {
  ping: () => "pong",
});
