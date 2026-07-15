import { create } from "zustand";
import { toast } from "sonner";
import { commands, events, type AutomationHookDetail, type AutomationHookInfo, type AutomationHookInput } from "./bindings";
import { LOCAL_RUNNER } from "./lib/session-key";

type AutomationsState = {
  hooks: AutomationHookInfo[];
  detailsById: Record<string, AutomationHookDetail>;
  loaded: boolean;
  load: () => Promise<void>;
  refresh: () => Promise<void>;
  loadDetail: (id: string) => Promise<AutomationHookDetail | null>;
  create: (input: AutomationHookInput) => Promise<AutomationHookInfo | null>;
  update: (id: string, input: AutomationHookInput) => Promise<AutomationHookInfo | null>;
  toggle: (id: string, enabled: boolean) => Promise<void>;
  remove: (id: string) => Promise<boolean>;
  testOutbound: (id: string) => Promise<AutomationHookDetail | null>;
};

function message(error: unknown): string {
  return error instanceof Error ? error.message : String(error);
}

let unlisten: (() => void) | null = null;
let listenPromise: Promise<void> | null = null;

export function resetAutomationListenerForTest(): void {
  unlisten?.();
  unlisten = null;
  listenPromise = null;
}

function ensureListener(get: () => AutomationsState): Promise<void> {
  if (unlisten) return Promise.resolve();
  if (listenPromise) return listenPromise;
  listenPromise = events.coreEventMsg
    .listen((event) => {
      if (event.payload.event.kind === "automationHookRunChanged") void get().refresh();
    })
    .then((stop) => {
      unlisten = stop;
    })
    .catch(() => {
      listenPromise = null;
    });
  return listenPromise;
}

export const useAutomations = create<AutomationsState>((set, get) => {
  const list = async (action: string) => {
    try {
      const result = await commands.listAutomationHooks(LOCAL_RUNNER);
      if (result.status === "ok") {
        set({ hooks: result.data, loaded: true });
        return;
      }
      toast.error(`${action}: ${result.error.message}`);
    } catch (error) {
      toast.error(`${action}: ${message(error)}`);
    }
  };

  const patchHook = (hook: AutomationHookInfo) =>
    set((state) => {
      const existing = state.hooks.find((item) => item.id === hook.id);
      const changed =
        !existing || Object.keys(hook).some((key) => existing[key as keyof AutomationHookInfo] !== hook[key as keyof AutomationHookInfo]);
      return {
        hooks: changed ? (existing ? state.hooks.map((item) => (item.id === hook.id ? hook : item)) : [hook, ...state.hooks]) : state.hooks,
        detailsById: state.detailsById[hook.id]
          ? { ...state.detailsById, [hook.id]: { ...state.detailsById[hook.id], hook } }
          : state.detailsById,
      };
    });

  return {
    hooks: [],
    detailsById: {},
    loaded: false,

    load: async () => {
      await list("Couldn't load hooks");
      await ensureListener(get);
    },

    refresh: async () => list("Couldn't refresh hooks"),

    loadDetail: async (id) => {
      try {
        const result = await commands.automationHookDetail(LOCAL_RUNNER, id);
        if (result.status !== "ok") {
          toast.error(`Couldn't load hook: ${result.error.message}`);
          return null;
        }
        set((state) => ({ detailsById: { ...state.detailsById, [id]: result.data } }));
        patchHook(result.data.hook);
        return result.data;
      } catch (error) {
        toast.error(`Couldn't load hook: ${message(error)}`);
        return null;
      }
    },

    create: async (input) => {
      try {
        const result = await commands.createAutomationHook(LOCAL_RUNNER, input);
        if (result.status !== "ok") {
          toast.error(`Couldn't create hook: ${result.error.message}`);
          return null;
        }
        patchHook(result.data);
        return result.data;
      } catch (error) {
        toast.error(`Couldn't create hook: ${message(error)}`);
        return null;
      }
    },

    update: async (id, input) => {
      try {
        const result = await commands.updateAutomationHook(LOCAL_RUNNER, id, input);
        if (result.status !== "ok") {
          toast.error(`Couldn't update hook: ${result.error.message}`);
          return null;
        }
        patchHook(result.data);
        return result.data;
      } catch (error) {
        toast.error(`Couldn't update hook: ${message(error)}`);
        return null;
      }
    },

    toggle: async (id, enabled) => {
      try {
        const result = await commands.toggleAutomationHook(LOCAL_RUNNER, id, enabled);
        if (result.status !== "ok") {
          toast.error(`Couldn't toggle hook: ${result.error.message}`);
          return;
        }
        patchHook(result.data);
      } catch (error) {
        toast.error(`Couldn't toggle hook: ${message(error)}`);
      }
    },

    remove: async (id) => {
      try {
        const result = await commands.deleteAutomationHook(LOCAL_RUNNER, id);
        if (result.status !== "ok") {
          toast.error(`Couldn't delete hook: ${result.error.message}`);
          return false;
        }
        set((state) => {
          const { [id]: _, ...detailsById } = state.detailsById;
          return { hooks: result.data, detailsById };
        });
        return true;
      } catch (error) {
        toast.error(`Couldn't delete hook: ${message(error)}`);
        return false;
      }
    },

    testOutbound: async (id) => {
      try {
        const result = await commands.testAutomationHook(LOCAL_RUNNER, id);
        if (result.status !== "ok") {
          toast.error(`Couldn't test delivery: ${result.error.message}`);
          return null;
        }
        set((state) => ({ detailsById: { ...state.detailsById, [id]: result.data } }));
        patchHook(result.data.hook);
        return result.data;
      } catch (error) {
        toast.error(`Couldn't test delivery: ${message(error)}`);
        return null;
      }
    },
  };
});
