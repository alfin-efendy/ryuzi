import { create } from "zustand";
import { commands } from "./bindings";
import type { ModelTestStatus } from "./lib/model-testing";

// App-wide model probe verdicts (docs/superpowers/specs/
// 2026-07-10-models-fixes-design.md, Part 2): hydrated once at app start and
// updated live by the Provider Models card's test flows, so every model
// picker can hide or flag invalid models. Pull + local update only — there
// is no push/event channel for statuses.

/** Composite map key. NUL cannot appear in a family or model id, so the
 *  pair round-trips unambiguously. */
export function statusKey(family: string, model: string): string {
  return `${family}\u0000${model}`;
}

type ModelStatusesState = {
  byKey: Record<string, ModelTestStatus>;
  hydrate: () => Promise<void>;
  upsert: (family: string, model: string, status: ModelTestStatus) => void;
};

export const useModelStatuses = create<ModelStatusesState>((set) => ({
  byKey: {},
  hydrate: async () => {
    const res = await commands.listAllModelStatuses("local");
    if (res.status !== "ok") return;
    const byKey: Record<string, ModelTestStatus> = {};
    for (const row of res.data) byKey[statusKey(row.family, row.model)] = row.status as ModelTestStatus;
    set({ byKey });
  },
  // Mirrors Store::upsert_model_status: "unknown" (rate limit / outage /
  // network) is transient and never overwrites a stored verdict.
  upsert: (family, model, status) => {
    if (status === "unknown") return;
    set((s) => ({ byKey: { ...s.byKey, [statusKey(family, model)]: status } }));
  },
}));
