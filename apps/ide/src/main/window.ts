// apps/ide/src/main/window.ts
import { app, BrowserWindow } from "electron";
import { join } from "node:path";

export function createWindow(): BrowserWindow {
  // NOTE: do not use `__dirname` to locate the bundles. `bun build --target=node`
  // inlines `__dirname` as a literal of the SOURCE file's directory (apps/ide/src/main),
  // so `join(__dirname, "../preload/...")` resolves into src/ at runtime and the built
  // dist/ files are never found (blank window / missing preload). Anchor on the app root
  // (the dir of package.json `main`), which is correct both unpackaged and packaged.
  const appRoot = app.getAppPath();
  const win = new BrowserWindow({
    width: 1200,
    height: 800,
    webPreferences: {
      preload: join(appRoot, "dist/preload/index.js"),
      contextIsolation: true,
      sandbox: true,
      nodeIntegration: false,
      webSecurity: true,
    },
  });
  win.loadFile(join(appRoot, "dist/renderer/index.html"));
  return win;
}
