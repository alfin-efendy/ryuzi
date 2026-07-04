import { create } from "zustand";
import { commands, type UsageSeries } from "./bindings";

// Usage charts on the Models screen: per-connection traffic + endpoint totals.

type UsageState = {
  byConnection: Record<string, UsageSeries>;
  endpoint: UsageSeries | null;
  loadConnection: (id: string, days?: number) => Promise<void>;
  loadEndpoint: (days?: number) => Promise<void>;
};

export const useUsage = create<UsageState>((set, get) => ({
  byConnection: {},
  endpoint: null,
  loadConnection: async (id, days = 14) => {
    const res = await commands.connectionUsage(id, days);
    if (res.status === "ok") set({ byConnection: { ...get().byConnection, [id]: res.data } });
  },
  loadEndpoint: async (days = 14) => {
    const res = await commands.endpointUsage(days);
    if (res.status === "ok") set({ endpoint: res.data });
  },
}));
