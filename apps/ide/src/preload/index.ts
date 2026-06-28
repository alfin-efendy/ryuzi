import { contextBridge, ipcRenderer } from "electron";
import { EVENT_CHANNEL, CONNECTION_CHANNEL, APPROVAL_CHANNEL, type HarnessBridge } from "../shared/ipc-contract";

const bridge: HarnessBridge = {
  listProjects: () => ipcRenderer.invoke("listProjects"),
  getProject: (id) => ipcRenderer.invoke("getProject", id),
  listSessions: (projectId) => ipcRenderer.invoke("listSessions", projectId),
  startSession: (req) => ipcRenderer.invoke("startSession", req),
  continueSession: (req) => ipcRenderer.invoke("continueSession", req),
  stopSession: (sessionPk) => ipcRenderer.invoke("stopSession", sessionPk),
  endSession: (sessionPk, opts) => ipcRenderer.invoke("endSession", sessionPk, opts),
  getConnId: () => ipcRenderer.invoke("getConnId"),
  onEvent: (cb) => {
    const handler = (_e: unknown, payload: Parameters<typeof cb>[0]) => cb(payload);
    ipcRenderer.on(EVENT_CHANNEL, handler);
    return () => ipcRenderer.removeListener(EVENT_CHANNEL, handler);
  },
  onConnectionChange: (cb) => {
    const handler = (_e: unknown, payload: Parameters<typeof cb>[0]) => cb(payload);
    ipcRenderer.on(CONNECTION_CHANNEL, handler);
    return () => ipcRenderer.removeListener(CONNECTION_CHANNEL, handler);
  },
  connectProject: (input) => ipcRenderer.invoke("connectProject", input),
  resolveApproval: (requestId, decision) => ipcRenderer.invoke("resolveApproval", requestId, decision),
  onApprovalRequest: (cb) => {
    const handler = (_e: unknown, payload: Parameters<typeof cb>[0]) => cb(payload);
    ipcRenderer.on(APPROVAL_CHANNEL, handler);
    return () => ipcRenderer.removeListener(APPROVAL_CHANNEL, handler);
  },
};

contextBridge.exposeInMainWorld("harness", bridge);
