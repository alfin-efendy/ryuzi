import { create } from "zustand";
import { toast } from "sonner";
import { commands, type InstalledSkillInfo } from "./bindings";

type SkillsState = {
  skills: InstalledSkillInfo[];
  loading: boolean;
  error: string | null;
  refresh: () => Promise<void>;
  refreshSkillPack: (id: string) => Promise<void>;
  remove: (id: string) => Promise<void>;
};

function trimMessage(error: string): string {
  return error.trim() === "" ? "Unknown error" : error;
}

export const useSkills = create<SkillsState>((set, get) => ({
  skills: [],
  loading: false,
  error: null,

  refresh: async () => {
    set({ loading: true, error: null });
    const res = await commands.listSkills();
    if (res.status === "ok") {
      set({ skills: res.data, loading: false, error: null });
      return;
    }

    const message = trimMessage(res.error);
    set({ loading: false, error: message });
    toast.error(`Skills failed: ${message}`);
  },

  refreshSkillPack: async (id) => {
    set({ loading: true, error: null });
    const res = await commands.refreshSkill(id);
    if (res.status === "error") {
      const message = trimMessage(res.error);
      set({ loading: false, error: message });
      toast.error(`Skill refresh failed: ${message}`);
      return;
    }

    toast.success(`${res.data.name} refreshed`);
    await get().refresh();
  },

  remove: async (id) => {
    set({ loading: true, error: null });
    const res = await commands.removeSkill(id);
    if (res.status === "error") {
      const message = trimMessage(res.error);
      set({ loading: false, error: message });
      toast.error(`Skill removal failed: ${message}`);
      return;
    }

    toast.success("Skill pack removed");
    await get().refresh();
  },
}));
