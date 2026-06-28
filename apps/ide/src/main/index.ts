// apps/ide/src/main/index.ts
import { app, BrowserWindow, shell, safeStorage } from "electron";
import { join } from "node:path";
import { createWindow } from "./window";
import { createSession } from "./client";
import { registerIpc, registerConnectionIpc } from "./ipc";
import { ConnectionsStore } from "./connections";
import { TokenStore } from "./token-store";
import { createOidcClient } from "./oidc";
import { ConnectionManager } from "./connection-manager";

let manager: ConnectionManager | null = null;

const gotLock = app.requestSingleInstanceLock();
if (!gotLock) {
  app.quit();
} else {
  app.whenReady().then(async () => {
    const win = createWindow();
    const send = (channel: string, payload: unknown) => {
      if (!win.isDestroyed()) win.webContents.send(channel, payload);
    };
    const store = new ConnectionsStore(join(app.getPath("userData"), "connections.json"));
    const tokens = new TokenStore(join(app.getPath("userData"), "tokens"), {
      isAvailable: () => safeStorage.isEncryptionAvailable(),
      encrypt: (s) => safeStorage.encryptString(s),
      decrypt: (b) => safeStorage.decryptString(b),
    });
    manager = new ConnectionManager({
      store,
      tokens,
      oidc: createOidcClient(),
      send,
      makeClient: (opts) => createSession(opts),
      openExternal: (url) => void shell.openExternal(url),
    });
    registerIpc(() => manager?.getClient() ?? null);
    registerConnectionIpc(manager);
    await manager.startup();
    app.on("activate", () => {
      if (BrowserWindow.getAllWindows().length === 0) createWindow();
    });
  });
  app.on("window-all-closed", () => {
    if (process.platform !== "darwin") app.quit();
  });
}
