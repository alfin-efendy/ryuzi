import { create } from "zustand";
import { toast } from "sonner";
import { commands, type AgentLearningInfo, type KnowledgeConceptInfo, type KnowledgeConceptMutationInfo } from "./bindings";

type LearningState = {
  byAgent: Record<string, AgentLearningInfo>;
  loading: Record<string, boolean>;
  rollingBack: Record<string, string | null>;
  requestGeneration: Record<string, number>;
  load: (agentId: string) => Promise<void>;
  createConcept: (agentId: string, input: KnowledgeConceptMutationInfo) => Promise<boolean>;
  updateConcept: (agentId: string, conceptId: string, input: KnowledgeConceptMutationInfo) => Promise<boolean>;
  deleteConcept: (agentId: string, conceptId: string) => Promise<boolean>;
  validateRaw: (agentId: string, path: string, raw: string) => Promise<KnowledgeConceptInfo | null>;
  replaceRaw: (agentId: string, path: string, raw: string) => Promise<boolean>;
  deleteInvalid: (agentId: string, path: string) => Promise<boolean>;
  rollback: (agentId: string, snapshotId: string) => Promise<boolean>;
};

const message = (resource: string) => `${resource} failed`;

export const useLearning = create<LearningState>((set, get) => {
  const installSnapshot = (agentId: string, snapshot: AgentLearningInfo) => {
    set((state) => ({
      byAgent: { ...state.byAgent, [agentId]: snapshot },
      loading: { ...state.loading, [agentId]: false },
      requestGeneration: { ...state.requestGeneration, [agentId]: (state.requestGeneration[agentId] ?? 0) + 1 },
    }));
  };

  return {
    byAgent: {},
    loading: {},
    rollingBack: {},
    requestGeneration: {},

    load: async (agentId) => {
      const generation = (get().requestGeneration[agentId] ?? 0) + 1;
      set((state) => ({
        loading: { ...state.loading, [agentId]: true },
        requestGeneration: { ...state.requestGeneration, [agentId]: generation },
      }));
      try {
        const result = await commands.getAgentLearning(agentId);
        if (get().requestGeneration[agentId] !== generation) return;
        if (result.status === "error") {
          toast.error(message("Learning load"));
          return;
        }
        set((state) => ({ byAgent: { ...state.byAgent, [agentId]: result.data } }));
      } finally {
        if (get().requestGeneration[agentId] === generation) {
          set((state) => ({ loading: { ...state.loading, [agentId]: false } }));
        }
      }
    },

    createConcept: async (agentId, input) => {
      const result = await commands.createAgentConcept(agentId, input);
      if (result.status === "error") {
        toast.error(message("Create memory"));
        return false;
      }
      await get().load(agentId);
      return true;
    },

    updateConcept: async (agentId, conceptId, input) => {
      const result = await commands.updateAgentConcept(agentId, conceptId, input);
      if (result.status === "error") {
        toast.error(message("Update memory"));
        return false;
      }
      await get().load(agentId);
      return true;
    },

    deleteConcept: async (agentId, conceptId) => {
      const result = await commands.deleteAgentConcept(agentId, conceptId);
      if (result.status === "error") {
        toast.error(message("Delete memory"));
        return false;
      }
      installSnapshot(agentId, result.data);
      return true;
    },

    validateRaw: async (agentId, path, raw) => {
      const result = await commands.validateAgentConceptRaw(agentId, path, raw);
      if (result.status === "error") {
        toast.error(message("Validate knowledge"));
        return null;
      }
      return result.data;
    },

    replaceRaw: async (agentId, path, raw) => {
      const result = await commands.replaceAgentConceptRaw(agentId, path, raw);
      if (result.status === "error") {
        toast.error(message("Replace knowledge"));
        return false;
      }
      await get().load(agentId);
      return true;
    },

    deleteInvalid: async (agentId, path) => {
      const result = await commands.deleteInvalidAgentConcept(agentId, path);
      if (result.status === "error") {
        toast.error(message("Delete invalid knowledge"));
        return false;
      }
      installSnapshot(agentId, result.data);
      return true;
    },

    rollback: async (agentId, snapshotId) => {
      set((state) => ({ rollingBack: { ...state.rollingBack, [agentId]: snapshotId } }));
      try {
        const result = await commands.rollbackAgentLearning(agentId, snapshotId);
        if (result.status === "error") {
          toast.error(message("Rollback knowledge"));
          return false;
        }
        installSnapshot(agentId, result.data);
        toast.success("Knowledge snapshot restored");
        return true;
      } finally {
        set((state) => ({ rollingBack: { ...state.rollingBack, [agentId]: null } }));
      }
    },
  };
});

/** Compact relative-time label retained for non-Learning activity rows. */
export function formatRelativeTime(ms: number, now: number = Date.now()): string {
  const seconds = Math.max(0, Math.round((now - ms) / 1000));
  if (seconds < 60) return "just now";
  const minutes = Math.round(seconds / 60);
  if (minutes < 60) return `${minutes}m ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours}h ago`;
  const days = Math.round(hours / 24);
  if (days < 30) return `${days}d ago`;
  const months = Math.round(days / 30);
  if (months < 12) return `${months}mo ago`;
  return `${Math.round(months / 12)}y ago`;
}
