import { create } from "zustand";
import { toast } from "sonner";
import { commands, type AddAppInput, type AppInfo, type CmdError, type Result } from "./bindings";

// Apps (MCP servers) domain store. Definitions persist in the engine; probes
// do a real MCP handshake; enabled servers attach to agent sessions for real.

type AppsState = {
  apps: AppInfo[];
  loaded: boolean;
  probing: string | null;
  hydrate: () => Promise<void>;
  add: (input: AddAppInput) => Promise<boolean>;
  remove: (id: string) => Promise<void>;
  probe: (id: string) => Promise<void>;
  setScope: (id: string, scope: string, scopeGateways: string[]) => Promise<void>;
  setToolPerm: (id: string, tool: string, perm: string) => Promise<void>;
  /** Allow/deny the (single, native) agent to use this app. */
  toggleAgent: (id: string, allowed: boolean) => Promise<void>;
};

function applyResult(set: (partial: Partial<AppsState>) => void, res: Result<AppInfo[], CmdError>, action: string): boolean {
  if (res.status === "ok") {
    set({ apps: res.data, loaded: true });
    return true;
  }
  toast.error(`${action} failed: ${res.error.message}`);
  return false;
}

export const useApps = create<AppsState>((set, get) => ({
  apps: [],
  loaded: false,
  probing: null,

  hydrate: async () => {
    applyResult(set, await commands.listApps(), "App list");
  },

  add: async (input) => applyResult(set, await commands.addApp(input), "Add app"),

  remove: async (id) => {
    applyResult(set, await commands.removeApp(id), "Remove app");
  },

  probe: async (id) => {
    set({ probing: id });
    try {
      applyResult(set, await commands.probeApp(id), "Probe");
    } finally {
      set({ probing: null });
    }
  },

  setScope: async (id, scope, scopeGateways) => {
    applyResult(set, await commands.updateAppScope(id, scope, scopeGateways), "Scope update");
  },

  setToolPerm: async (id, tool, perm) => {
    set({
      apps: get().apps.map((a) => (a.id === id ? { ...a, tools: a.tools.map((t) => (t.name === tool ? { ...t, perm } : t)) } : a)),
    });
    applyResult(set, await commands.setAppToolPerm(id, tool, perm), "Tool permission");
  },

  toggleAgent: async (id, allowed) => {
    set({
      apps: get().apps.map((a) =>
        a.id === id ? { ...a, agentAccess: a.agentAccess.map((x) => (x.agentId === "native" ? { ...x, allowed } : x)) } : a,
      ),
    });
    applyResult(set, await commands.toggleAppAgent(id, "native", allowed), "Agent access");
  },
}));

export function appById(apps: AppInfo[], id: string): AppInfo | undefined {
  return apps.find((a) => a.id === id);
}

/** Whether the native agent may use this app. Missing row = allowed. */
export function agentAllowed(app: AppInfo): boolean {
  return app.agentAccess.find((x) => x.agentId === "native")?.allowed ?? true;
}
