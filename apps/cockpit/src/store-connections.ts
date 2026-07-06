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

// Providers tab: catalog + credentialed provider connections.

type ConnectionPatch = {
  label: string;
  enabled: boolean;
  apiKey: string | null;
  baseUrl: string | null;
  models: string[];
  claudeCloaking: boolean | null;
};

type ConnectionsState = {
  catalog: CatalogEntry[];
  connections: ConnectionInfo[];
  loaded: boolean;
  hydrate: () => Promise<void>;
  add: (provider: string, label: string, apiKey: string, baseUrl: string | null) => Promise<boolean>;
  update: (id: string, patch: ConnectionPatch) => Promise<void>;
  remove: (id: string) => Promise<void>;
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
};

function apply(set: (p: Partial<ConnectionsState>) => void, res: Result<ConnectionInfo[], CmdError>, action: string): boolean {
  if (res.status === "ok") {
    set({ connections: res.data });
    return true;
  }
  toast.error(`${action} failed: ${res.error.message}`);
  return false;
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
  update: async (id, p) =>
    void apply(
      set,
      await commands.updateConnection(id, p.label, p.enabled, p.apiKey, p.baseUrl, p.models, p.claudeCloaking),
      "Update connection",
    ),
  remove: async (id) => void apply(set, await commands.removeConnection(id), "Remove connection"),
  move: async (id, dir) => void apply(set, await commands.moveConnection(id, dir), "Reorder"),
  test: async (id) => {
    const res = await commands.testConnection(id);
    if (res.status === "ok") return res.data;
    toast.error(`Test failed: ${res.error.message}`);
    return null;
  },
  connectOauth: async (provider, label) => apply(set, await commands.connectOauth(provider, label), "Connect"),
  reconnectOauth: async (connectionId) => apply(set, await commands.reconnectOauth(connectionId), "Reconnect"),
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
}));
