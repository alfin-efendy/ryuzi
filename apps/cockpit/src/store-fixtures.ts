import { create } from "zustand";
import {
  AGENTS,
  APPS,
  PROVIDERS,
  SCHEDULE_JOBS,
  type AgentId,
  type AppFixture,
  type JobFixture,
  type PermMode,
  type ProviderFixture,
} from "./fixtures";

// Interactive state over the design-preview fixtures. Everything here is
// session-local: it makes the new screens behave like the design prototype
// until real provider/agent/scheduler/app backends land.

export type AgentState = { enabled: boolean; model: string; permMode: PermMode; flags: string; version: string };
export type ProviderState = {
  on: boolean;
  failAuto: boolean;
  threshold: number;
  returnToPrimary: boolean;
  activeAccount: string;
  accountOrder: string[];
};

type FixtureState = {
  defaultAgent: AgentId;
  agentState: Record<AgentId, AgentState>;
  providerState: Record<string, ProviderState>;
  jobs: JobFixture[];
  apps: AppFixture[];
  registryState: Record<string, "installing" | "installed">;
  activeWorkspace: string;

  setDefaultAgent: (id: AgentId) => void;
  toggleAgent: (id: AgentId) => void;
  setAgentModel: (id: AgentId, model: string) => void;
  setAgentPerm: (id: AgentId, mode: PermMode) => void;
  setAgentFlags: (id: AgentId, flags: string) => void;
  applyAgentUpdate: (id: AgentId) => void;
  setAgentAppAccess: (agentId: AgentId, appId: string, on: boolean) => void;

  toggleProvider: (id: string) => void;
  setFailAuto: (id: string, on: boolean) => void;
  setThreshold: (id: string, pct: number) => void;
  setReturnToPrimary: (id: string, on: boolean) => void;
  setActiveAccount: (id: string, accountId: string) => void;
  moveAccount: (id: string, accountId: string, dir: -1 | 1) => void;

  toggleJob: (id: string) => void;
  updateJob: (id: string, patch: Partial<JobFixture>) => void;
  createJob: (job: JobFixture) => void;

  setAppScope: (id: string, scope: "global" | "select") => void;
  toggleAppWs: (id: string, ws: string) => void;
  setToolPerm: (id: string, tool: string, perm: "allow" | "ask" | "deny") => void;
  toggleAppAgent: (id: string, agent: AgentId) => void;
  uninstallApp: (id: string) => void;
  installRegistry: (id: string) => void;
  setActiveWorkspace: (id: string) => void;
};

function initialAgentState(): Record<AgentId, AgentState> {
  const out = {} as Record<AgentId, AgentState>;
  for (const a of Object.values(AGENTS)) {
    out[a.id] = { enabled: a.id !== "local", model: a.model, permMode: a.permMode, flags: a.flags, version: a.version };
  }
  return out;
}

function initialProviderState(): Record<string, ProviderState> {
  const out: Record<string, ProviderState> = {};
  for (const p of PROVIDERS) {
    out[p.id] = {
      on: p.id !== "local",
      failAuto: p.failover.auto,
      threshold: p.failover.threshold,
      returnToPrimary: p.failover.returnToPrimary,
      activeAccount: p.accounts[0]?.id ?? "",
      accountOrder: p.accounts.map((a) => a.id),
    };
  }
  return out;
}

const mapAgents = (st: FixtureState, id: AgentId, patch: Partial<AgentState>) => ({
  agentState: { ...st.agentState, [id]: { ...st.agentState[id], ...patch } },
});

const mapProvider = (st: FixtureState, id: string, patch: Partial<ProviderState>) => ({
  providerState: { ...st.providerState, [id]: { ...st.providerState[id], ...patch } },
});

const mapApp = (st: FixtureState, id: string, fn: (a: AppFixture) => AppFixture) => ({
  apps: st.apps.map((a) => (a.id === id ? fn(a) : a)),
});

export const useFixtures = create<FixtureState>((set) => ({
  defaultAgent: "claude",
  agentState: initialAgentState(),
  providerState: initialProviderState(),
  jobs: SCHEDULE_JOBS,
  apps: APPS,
  registryState: {},
  activeWorkspace: "local",

  setDefaultAgent: (id) => set({ defaultAgent: id }),
  toggleAgent: (id) => set((st) => mapAgents(st, id, { enabled: !st.agentState[id].enabled })),
  setAgentModel: (id, model) => set((st) => mapAgents(st, id, { model })),
  setAgentPerm: (id, permMode) => set((st) => mapAgents(st, id, { permMode })),
  setAgentFlags: (id, flags) => set((st) => mapAgents(st, id, { flags })),
  applyAgentUpdate: (id) => set((st) => mapAgents(st, id, { version: AGENTS[id].latest })),
  setAgentAppAccess: (agentId, appId, on) =>
    set((st) => ({
      apps: st.apps.map((a) => (a.id === appId ? { ...a, agentAccess: { ...a.agentAccess, [agentId]: on } } : a)),
    })),

  toggleProvider: (id) => set((st) => mapProvider(st, id, { on: !st.providerState[id].on })),
  setFailAuto: (id, on) => set((st) => mapProvider(st, id, { failAuto: on })),
  setThreshold: (id, pct) => set((st) => mapProvider(st, id, { threshold: pct })),
  setReturnToPrimary: (id, on) => set((st) => mapProvider(st, id, { returnToPrimary: on })),
  setActiveAccount: (id, accountId) => set((st) => mapProvider(st, id, { activeAccount: accountId })),
  moveAccount: (id, accountId, dir) =>
    set((st) => {
      const order = [...st.providerState[id].accountOrder];
      const i = order.indexOf(accountId);
      const j = i + dir;
      if (i === -1 || j < 0 || j >= order.length) return {};
      [order[i], order[j]] = [order[j], order[i]];
      return mapProvider(st, id, { accountOrder: order });
    }),

  toggleJob: (id) => set((st) => ({ jobs: st.jobs.map((j) => (j.id === id ? { ...j, on: !j.on } : j)) })),
  updateJob: (id, patch) => set((st) => ({ jobs: st.jobs.map((j) => (j.id === id ? { ...j, ...patch } : j)) })),
  createJob: (job) => set((st) => ({ jobs: [job, ...st.jobs] })),

  setAppScope: (id, scope) => set((st) => mapApp(st, id, (a) => ({ ...a, scope }))),
  toggleAppWs: (id, ws) => set((st) => mapApp(st, id, (a) => ({ ...a, scopeWs: { ...a.scopeWs, [ws]: !a.scopeWs[ws] } }))),
  setToolPerm: (id, tool, perm) =>
    set((st) => mapApp(st, id, (a) => ({ ...a, tools: a.tools.map((t) => (t.name === tool ? { ...t, perm } : t)) }))),
  toggleAppAgent: (id, agent) =>
    set((st) => mapApp(st, id, (a) => ({ ...a, agentAccess: { ...a.agentAccess, [agent]: !a.agentAccess[agent] } }))),
  uninstallApp: (id) => set((st) => ({ apps: st.apps.filter((a) => a.id !== id) })),
  installRegistry: (id) => {
    set((st) => ({ registryState: { ...st.registryState, [id]: "installing" } }));
    setTimeout(() => {
      set((st) => ({ registryState: { ...st.registryState, [id]: "installed" } }));
    }, 1400);
  },
  setActiveWorkspace: (id) => set({ activeWorkspace: id }),
}));
