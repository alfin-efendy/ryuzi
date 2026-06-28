// apps/ide/src/main/window.ts
import { BrowserWindow } from "electron";
import { join } from "node:path";

export function createWindow(): BrowserWindow {
  const win = new BrowserWindow({
    width: 1200,
    height: 800,
    webPreferences: {
      preload: join(__dirname, "../preload/index.js"),
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      webSecurity: true,
    },
  });
  win.loadFile(join(__dirname, "../renderer/index.html"));
  return win;
}
