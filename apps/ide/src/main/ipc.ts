// apps/ide/src/main/ipc.ts
import { ipcMain } from "electron";
import type { RemoteControlPlane } from "@harness/client";
import type { ConnectionManager } from "./connection-manager";
import type { AddConnectionInput } from "../shared/ipc-contract";

export function registerIpc(getClient: () => RemoteControlPlane | null): void {
  const need = (): RemoteControlPlane => {
    const c = getClient();
    if (!c) throw new Error("router not connected");
    return c;
  };
  ipcMain.handle("listProjects", async () => need().listProjects());
  ipcMain.handle("getProject", async (_e, id: string) => need().getProject(id));
  ipcMain.handle("listSessions", async (_e, projectId?: string) => need().listSessions(projectId));
  ipcMain.handle("startSession", async (_e, req) => need().startSession(req));
  ipcMain.handle("continueSession", async (_e, req) => need().continueSession(req));
  ipcMain.handle("stopSession", async (_e, sessionPk: string) => need().stopSession(sessionPk));
  ipcMain.handle("endSession", async (_e, sessionPk: string, opts?: { keepBranch?: boolean }) => need().endSession(sessionPk, opts));
  ipcMain.handle("getConnId", async () => getClient()?.connId ?? null);
  ipcMain.handle("connectProject", async (_e, input: { gitUrl?: string; name?: string }) =>
    need().connectProject({ gateway: "ide", workspaceId: crypto.randomUUID(), name: input.name, gitUrl: input.gitUrl }),
  );
  ipcMain.handle("resolveApproval", async (_e, requestId: string, decision: "allow" | "deny") => {
    getClient()?.resolveApproval(requestId, decision);
  });
  ipcMain.handle("listDir", async (_e, req: { sessionPk: string; path: string }) => need().listDir(req));
  ipcMain.handle("readFile", async (_e, req: { sessionPk: string; path: string }) => need().readFile(req));
}

export function registerConnectionIpc(manager: ConnectionManager): void {
  ipcMain.handle("listConnections", async () => manager.list());
  ipcMain.handle("addConnection", async (_e, input: AddConnectionInput) => manager.add(input));
  ipcMain.handle("removeConnection", async (_e, id: string) => manager.remove(id));
  ipcMain.handle("selectConnection", async (_e, id: string) => manager.select(id));
  ipcMain.handle("signIn", async (_e, id: string) => manager.signIn(id));
  ipcMain.handle("signOut", async (_e, id: string) => manager.signOut(id));
}
