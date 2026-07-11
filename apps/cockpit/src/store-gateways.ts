import { create } from "zustand";
import { toast } from "sonner";
import { commands, type CmdError, type GatewayEventInfo, type GatewayInfo, type Result } from "./bindings";
import { useStore } from "./store";

// Gateways domain store. The local host always exists (live telemetry); WSL
// distros are detected; SSH remotes are persisted config with a TCP probe.

const KEY_ACTIVE = "cockpit.gateways.active";

type GatewaysState = {
  gateways: GatewayInfo[];
  eventsById: Record<string, GatewayEventInfo[]>;
  activeGateway: string;
  loaded: boolean;
  probing: boolean;
  hydrate: () => Promise<void>;
  probe: () => Promise<void>;
  add: (name: string, host: string, port: number, username: string) => Promise<boolean>;
  addRunner: (name: string, host: string, port: number, fingerprint: string, code: string) => Promise<boolean>;
  remove: (id: string) => Promise<void>;
  updateFs: (id: string, fsMode: string, paths: string[]) => Promise<void>;
  loadEvents: (id: string) => Promise<void>;
  setActive: (id: string) => void;
};

function applyResult(set: (partial: Partial<GatewaysState>) => void, res: Result<GatewayInfo[], CmdError>, action: string): boolean {
  if (res.status === "ok") {
    set({ gateways: res.data, loaded: true });
    return true;
  }
  toast.error(`${action} failed: ${res.error.message}`);
  return false;
}

export const useGateways = create<GatewaysState>((set, get) => ({
  gateways: [],
  eventsById: {},
  activeGateway: (typeof localStorage !== "undefined" && localStorage.getItem(KEY_ACTIVE)) || "local",
  loaded: false,
  probing: false,

  hydrate: async () => {
    applyResult(set, await commands.listGateways("local"), "Gateway list");
    void get().probe();
  },

  probe: async () => {
    if (get().probing) return;
    set({ probing: true });
    try {
      applyResult(set, await commands.probeGateways("local"), "Gateway probe");
    } finally {
      set({ probing: false });
    }
  },

  add: async (name, host, port, username) => {
    const ok = applyResult(set, await commands.addGateway("local", name, host, port, username), "Add gateway");
    return ok;
  },

  // Pairs a remote runner over pinned TLS and persists + live-adds it — all
  // handled backend-side (Cockpit's `add_runner` Tauri command); the device
  // token it mints along the way never crosses into this store or the webview.
  addRunner: async (name, host, port, fingerprint, code) => {
    const ok = applyResult(set, await commands.addRunner(name, host, port, fingerprint, code), "Add runner");
    return ok;
  },

  remove: async (id) => {
    const ok = applyResult(set, await commands.removeGateway("local", id), "Remove gateway");
    // The Tauri command already aborted the runner's SSE bridge and dropped
    // its EngineClient backend-side (`EngineManager::remove_runner`); the
    // removed runner's sessions vanish from `useStore.sessions` on the next
    // `refresh()` (it re-fetches from `listGateways`, so a gone runner is
    // simply not in the fan-out list anymore). `transcripts`/`lastSeq` are
    // keyed by `sessKey(runnerId, pk)` and never otherwise get an eviction
    // pass, so prune this runner's entries here too rather than leaving them
    // as orphaned memory for the rest of the session. (A few smaller
    // per-session maps — `loaded`, `contextUsage`, `sessionCost` — and the
    // `pendingApprovals` array are left as-is: same orphaned-but-harmless
    // shape, not worth the extra surface for this fix.)
    if (ok) {
      const prefix = `${id}::`;
      useStore.setState((st) => ({
        transcripts: Object.fromEntries(Object.entries(st.transcripts).filter(([k]) => !k.startsWith(prefix))),
        lastSeq: Object.fromEntries(Object.entries(st.lastSeq).filter(([k]) => !k.startsWith(prefix))),
      }));
    }
  },

  updateFs: async (id, fsMode, paths) => {
    set({
      gateways: get().gateways.map((g) => (g.id === id ? { ...g, fsMode, paths } : g)),
    });
    applyResult(set, await commands.updateGateway("local", id, fsMode, paths), "Gateway update");
  },

  loadEvents: async (id) => {
    const res = await commands.gatewayEvents("local", id);
    if (res.status === "ok") set({ eventsById: { ...get().eventsById, [id]: res.data } });
  },

  setActive: (id) => {
    if (typeof localStorage !== "undefined") localStorage.setItem(KEY_ACTIVE, id);
    set({ activeGateway: id });
  },
}));

export function gatewayById(gateways: GatewayInfo[], id: string): GatewayInfo | undefined {
  return gateways.find((g) => g.id === id);
}

/** "now", "5m ago", "2h 14m ago", "3d ago" from an epoch-ms timestamp. */
export function formatLastSeen(ms: number | null): string {
  if (ms === null) return "never";
  const delta = Math.max(0, Date.now() - ms);
  const mins = Math.floor(delta / 60_000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m ago`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h ${mins % 60}m ago`;
  return `${Math.floor(hours / 24)}d ago`;
}

/** Log line color by event level (matches the transcript palette). */
export function eventColor(level: string): string {
  if (level === "error") return "#EF4444";
  if (level === "warn") return "#F59E0B";
  if (level === "success") return "#22C55E";
  return "var(--code-foreground)";
}
