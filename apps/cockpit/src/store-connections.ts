import { create } from "zustand";
import { toast } from "sonner";
import {
  commands,
  type CatalogEntry,
  type ConnectionInfo,
  type DeviceFlowInfo,
  type ManualStartInfo,
  type TestResult,
  type Result,
  type CmdError,
} from "./bindings";
import { KIRO_IMPORT_ACTION, KIRO_SIGNIN_ACTION } from "./constants";
import { useStore } from "./store";

// Providers tab: catalog + credentialed provider connections.

type ConnectionsState = {
  catalog: CatalogEntry[];
  connections: ConnectionInfo[];
  loaded: boolean;
  hydrate: () => Promise<void>;
  add: (provider: string, label: string, apiKey: string, baseUrl: string | null) => Promise<boolean>;
  rename: (id: string, label: string) => Promise<boolean>;
  setEnabled: (id: string, enabled: boolean) => Promise<boolean>;
  remove: (id: string) => Promise<boolean>;
  move: (id: string, dir: number) => Promise<void>;
  test: (id: string) => Promise<TestResult | null>;
  connectOauth: (provider: string, label: string) => Promise<boolean>;
  reconnectOauth: (connectionId: string) => Promise<boolean>;
  beginOauthManual: (provider: string) => Promise<ManualStartInfo | null>;
  completeOauthManual: (
    provider: string,
    label: string,
    verifier: string,
    state: string,
    pasted: string,
    redirectUri: string,
  ) => Promise<boolean>;
  addFree: (provider: string, label: string) => Promise<boolean>;
  startKiroDevice: () => Promise<DeviceFlowInfo | null>;
  awaitKiroDevice: (label: string, flowId: string) => Promise<boolean>;
  importKiro: (label: string) => Promise<boolean>;
  startDeviceFlow: (provider: string) => Promise<DeviceFlowInfo | null>;
  awaitDeviceFlow: (provider: string, label: string, flowId: string) => Promise<boolean>;
};

async function apply(
  set: (p: Partial<ConnectionsState>) => void,
  res: Result<ConnectionInfo[], CmdError>,
  action: string,
): Promise<boolean> {
  if (res.status === "ok") {
    set({ connections: res.data });
    await useStore.getState().refreshModelConfiguration();
    return true;
  }
  toast.error(`${action} failed: ${res.error.message}`);
  return false;
}

async function runAccountAction(
  set: (p: Partial<ConnectionsState>) => void,
  command: Promise<Result<ConnectionInfo[], CmdError>>,
  action: string,
): Promise<boolean> {
  try {
    return await apply(set, await command, action);
  } catch (error) {
    toast.error(`${action} failed: ${error instanceof Error ? error.message : String(error)}`);
    return false;
  }
}

export const useConnections = create<ConnectionsState>((set) => ({
  catalog: [],
  connections: [],
  loaded: false,

  hydrate: async () => {
    const [cat, conns] = await Promise.all([commands.listProviderCatalog(), commands.listConnections()]);
    if (cat.status === "ok") set({ catalog: cat.data });
    if (conns.status === "ok") set({ connections: conns.data });
    set({ loaded: true });
  },
  add: async (provider, label, apiKey, baseUrl) =>
    apply(set, await commands.addConnection(provider, label, apiKey, baseUrl), "Add connection"),
  rename: async (id, label) => runAccountAction(set, commands.renameConnection(id, label), "Rename account"),
  setEnabled: async (id, enabled) => runAccountAction(set, commands.setConnectionEnabled(id, enabled), "Update account"),
  remove: async (id) => runAccountAction(set, commands.removeConnection(id), "Remove account"),
  move: async (id, dir) => {
    await apply(set, await commands.moveConnection(id, dir), "Reorder");
  },
  test: async (id) => {
    const res = await commands.testConnection(id);
    if (res.status === "ok") return res.data;
    toast.error(`Test failed: ${res.error.message}`);
    return null;
  },
  connectOauth: async (provider, label) => apply(set, await commands.connectOauth(provider, label), "Connect"),
  reconnectOauth: async (connectionId) => runAccountAction(set, commands.reconnectOauth(connectionId), "Reconnect"),
  beginOauthManual: async (provider) => {
    const res = await commands.beginOauthManual(provider);
    if (res.status === "ok") return res.data;
    toast.error(`Connect failed: ${res.error.message}`);
    return null;
  },
  completeOauthManual: async (provider, label, verifier, state, pasted, redirectUri) =>
    apply(set, await commands.completeOauthManual(provider, label, verifier, state, pasted, redirectUri), "Connect"),
  addFree: async (provider, label) => apply(set, await commands.addFreeConnection(provider, label), "Add connection"),
  startKiroDevice: async () => {
    const res = await commands.startKiroDeviceFlow();
    if (res.status === "ok") return res.data;
    toast.error(`${KIRO_SIGNIN_ACTION} failed: ${res.error.message}`);
    return null;
  },
  awaitKiroDevice: async (label, flowId) => apply(set, await commands.awaitKiroDeviceFlow(label, flowId), KIRO_SIGNIN_ACTION),
  importKiro: async (label) => apply(set, await commands.importKiroToken(label), KIRO_IMPORT_ACTION),
  startDeviceFlow: async (provider) => {
    const res = await commands.startDeviceFlow(provider);
    if (res.status === "ok") return res.data;
    toast.error(`Sign in failed: ${res.error.message}`);
    return null;
  },
  awaitDeviceFlow: async (provider, label, flowId) => apply(set, await commands.awaitDeviceFlow(provider, label, flowId), "Sign in"),
}));
