import { create } from "zustand";
import { toast } from "sonner";
import { commands, type CatalogEntry, type ConnectionInfo, type TestResult, type Result, type CmdError } from "./bindings";

// Providers tab: catalog + credentialed provider connections.

type ConnectionPatch = {
  label: string;
  enabled: boolean;
  apiKey: string | null;
  baseUrl: string | null;
  models: string[];
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
    void apply(set, await commands.updateConnection(id, p.label, p.enabled, p.apiKey, p.baseUrl, p.models), "Update connection"),
  remove: async (id) => void apply(set, await commands.removeConnection(id), "Remove connection"),
  move: async (id, dir) => void apply(set, await commands.moveConnection(id, dir), "Reorder"),
  test: async (id) => {
    const res = await commands.testConnection(id);
    if (res.status === "ok") return res.data;
    toast.error(`Test failed: ${res.error.message}`);
    return null;
  },
}));
