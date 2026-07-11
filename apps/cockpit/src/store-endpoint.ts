import { create } from "zustand";
import { toast } from "sonner";
import { commands, type EndpointStatusInfo, type EndpointKeyInfo, type Result, type CmdError } from "./bindings";

// Endpoint tab: local router server lifecycle + endpoint API keys.

type EndpointState = {
  status: EndpointStatusInfo | null;
  keys: EndpointKeyInfo[];
  loaded: boolean;
  hydrate: () => Promise<void>;
  start: () => Promise<void>;
  stop: () => Promise<void>;
  setConfig: (port: number, autostart: boolean) => Promise<void>;
  createKey: (name: string) => Promise<void>;
  revokeKey: (id: string) => Promise<void>;
};

function applyStatus(set: (p: Partial<EndpointState>) => void, res: Result<EndpointStatusInfo, CmdError>, action: string) {
  if (res.status === "ok") set({ status: res.data });
  else toast.error(`${action} failed: ${res.error.message}`);
}

function applyKeys(set: (p: Partial<EndpointState>) => void, res: Result<EndpointKeyInfo[], CmdError>, action: string) {
  if (res.status === "ok") set({ keys: res.data });
  else toast.error(`${action} failed: ${res.error.message}`);
}

export const useEndpoint = create<EndpointState>((set) => ({
  status: null,
  keys: [],
  loaded: false,

  hydrate: async () => {
    const [st, ks] = await Promise.all([commands.endpointStatus("local"), commands.listEndpointKeys("local")]);
    if (st.status === "ok") set({ status: st.data });
    if (ks.status === "ok") set({ keys: ks.data });
    set({ loaded: true });
  },
  start: async () => applyStatus(set, await commands.startEndpoint("local"), "Start endpoint"),
  stop: async () => applyStatus(set, await commands.stopEndpoint("local"), "Stop endpoint"),
  setConfig: async (port, autostart) => applyStatus(set, await commands.setEndpointConfig("local", port, autostart), "Endpoint config"),
  createKey: async (name) => applyKeys(set, await commands.createEndpointKey("local", name), "Create key"),
  revokeKey: async (id) => applyKeys(set, await commands.revokeEndpointKey("local", id), "Revoke key"),
}));
