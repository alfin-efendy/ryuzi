import { create } from "zustand";
import { toast } from "sonner";
import { commands, type CmdError, type ProviderInfo, type Result } from "./bindings";

// Providers domain store: persisted provider/account config (rotation,
// failover, user-set limits) + usage aggregated from the local transcript DB.

type ProvidersState = {
  providers: ProviderInfo[];
  loaded: boolean;
  hydrate: () => Promise<void>;
  add: (id: string, name: string, kind: string, color: string) => Promise<boolean>;
  remove: (id: string) => Promise<void>;
  update: (
    id: string,
    patch: Partial<Pick<ProviderInfo, "enabled" | "strategy" | "failAuto" | "threshold" | "returnToPrimary">>,
  ) => Promise<void>;
  addAccount: (
    providerId: string,
    label: string,
    email: string,
    plan: string,
    sessionLimit: number | null,
    weeklyLimit: number | null,
  ) => Promise<boolean>;
  removeAccount: (accountId: string) => Promise<void>;
  setActiveAccount: (providerId: string, accountId: string) => Promise<void>;
  moveAccount: (providerId: string, accountId: string, dir: -1 | 1) => Promise<void>;
};

function applyResult(set: (partial: Partial<ProvidersState>) => void, res: Result<ProviderInfo[], CmdError>, action: string): boolean {
  if (res.status === "ok") {
    set({ providers: res.data, loaded: true });
    return true;
  }
  toast.error(`${action} failed: ${res.error.message}`);
  return false;
}

export const useProviders = create<ProvidersState>((set, get) => ({
  providers: [],
  loaded: false,

  hydrate: async () => {
    applyResult(set, await commands.listProviders(), "Provider list");
  },

  add: async (id, name, kind, color) => applyResult(set, await commands.addProvider(id, name, kind, color), "Add provider"),

  remove: async (id) => {
    applyResult(set, await commands.removeProvider(id), "Remove provider");
  },

  update: async (id, patch) => {
    const current = get().providers.find((p) => p.id === id);
    if (!current) return;
    const next = { ...current, ...patch };
    set({ providers: get().providers.map((p) => (p.id === id ? next : p)) });
    applyResult(
      set,
      await commands.updateProvider(id, next.enabled, next.strategy, next.failAuto, next.threshold, next.returnToPrimary),
      "Provider update",
    );
  },

  addAccount: async (providerId, label, email, plan, sessionLimit, weeklyLimit) =>
    applyResult(set, await commands.addProviderAccount(providerId, label, email, plan, sessionLimit, weeklyLimit), "Add account"),

  removeAccount: async (accountId) => {
    applyResult(set, await commands.removeProviderAccount(accountId), "Remove account");
  },

  setActiveAccount: async (providerId, accountId) => {
    applyResult(set, await commands.setActiveAccount(providerId, accountId), "Set active account");
  },

  moveAccount: async (providerId, accountId, dir) => {
    applyResult(set, await commands.moveProviderAccount(providerId, accountId, dir), "Reorder accounts");
  },
}));

export function providerById(providers: ProviderInfo[], id: string): ProviderInfo | undefined {
  return providers.find((p) => p.id === id);
}
