import { create } from "zustand";
import { toast } from "sonner";
import {
  commands,
  type CatalogEntry,
  type ConnectionInfo,
  type DeviceFlowInfo,
  type ManualStartInfo,
  type TestResult,
  type Result,
  type CmdError,
} from "./bindings";
import { KIRO_IMPORT_ACTION, KIRO_SIGNIN_ACTION } from "./constants";
import { LOCAL_RUNNER } from "./lib/session-key";

// Providers tab: catalog + credentialed provider connections.
//
// Runner target: every command below is pinned to `LOCAL_RUNNER`. This is
// intentional, not a leftover — there is no remote-runner Providers UI yet
// (a per-runner runner selector for this tab is a follow-up), and
// connections are correctly scoped per-runner regardless: each engine
// (local or remote) owns its own credential store, so "Providers" always
// means "the local engine's providers" until that selector exists.
//
// Remote OAuth support matrix (see also `engine_manager.rs::spawn_bridge`,
// which already opens `oauthAuthorizeUrl` / `pluginOauthAuthorizeUrl` in the
// CLIENT-side browser via `tauri_plugin_opener::open_url`, once per runner —
// so "the client browses to the authorize URL" is already remote-safe for
// every flow below, including on a future remote runner):
//   - Device flow (Qwen, GitHub Copilot, Kiro): daemon requests a device
//     code over outbound HTTPS and polls the token endpoint — no loopback,
//     already machine-independent. Remote-safe.
//   - Manual paste (anthropic-oauth, RedirectMode::LoopbackRandom in
//     `registry.rs`): `beginOauthManual` builds the URL + PKCE
//     Cockpit-side, the user pastes the code, `completeOauthManual` is
//     daemon-proxied. Remote-safe.
//   - Plugin OAuth: Cockpit binds the loopback callback client-side
//     (port 8976); the daemon only builds the authorize URL. Remote-safe.
//   - `connectOauth` / `reconnectOauth` (interactive loopback flow, e.g.
//     anthropic-oauth): the loopback listener is bound BY THE DAEMON
//     (`registry.rs` `RedirectMode::LoopbackRandom`), so on a remote runner
//     the browser's redirect would hit the client's own localhost, where
//     nothing is listening. Local-runner-only for now — splitting this
//     into a client-side-loopback flow is a larger future change, not done
//     here.
//   - LoopbackFixed providers (openai-oauth, fixed port 1455 — see
//     `RedirectMode::LoopbackFixed` in `registry.rs`): same daemon-side
//     loopback problem as above, AND manual paste bails for fixed-port
//     redirects (see `begin_oauth_manual`'s doc comment), AND there's no
//     device flow for these providers. This is the one OAuth class with no
//     remote-safe path at all today. If/when a remote Providers selector is
//     built, fixed-port providers must stay gated to the local runner (or
//     gain a dedicated remote-loopback-forwarding flow) — do not wire
//     `connectOauth`/`reconnectOauth` for them against a remote `runnerId`
//     without solving that first.

type ConnectionPatch = {
  label: string;
  enabled: boolean;
  apiKey: string | null;
  baseUrl: string | null;
  models: string[];
  claudeCloaking: boolean | null;
};

type ConnectionsState = {
  catalog: CatalogEntry[];
  connections: ConnectionInfo[];
  loaded: boolean;
  hydrate: () => Promise<void>;
  add: (provider: string, label: string, apiKey: string, baseUrl: string | null) => Promise<boolean>;
  update: (id: string, patch: ConnectionPatch) => Promise<void>;
  remove: (id: string) => Promise<void>;
  move: (id: string, dir: number) => Promise<void>;
  test: (id: string) => Promise<TestResult | null>;
  connectOauth: (provider: string, label: string) => Promise<boolean>;
  reconnectOauth: (connectionId: string) => Promise<boolean>;
  beginOauthManual: (provider: string) => Promise<ManualStartInfo | null>;
  completeOauthManual: (
    provider: string,
    label: string,
    verifier: string,
    state: string,
    pasted: string,
    redirectUri: string,
  ) => Promise<boolean>;
  addFree: (provider: string, label: string) => Promise<boolean>;
  startKiroDevice: () => Promise<DeviceFlowInfo | null>;
  awaitKiroDevice: (label: string, flowId: string) => Promise<boolean>;
  importKiro: (label: string) => Promise<boolean>;
  startDeviceFlow: (provider: string) => Promise<DeviceFlowInfo | null>;
  awaitDeviceFlow: (provider: string, label: string, flowId: string) => Promise<boolean>;
};

function apply(set: (p: Partial<ConnectionsState>) => void, res: Result<ConnectionInfo[], CmdError>, action: string): boolean {
  if (res.status === "ok") {
    set({ connections: res.data });
    return true;
  }
  toast.error(`${action} failed: ${res.error.message}`);
  return false;
}

export const useConnections = create<ConnectionsState>((set) => ({
  catalog: [],
  connections: [],
  loaded: false,

  hydrate: async () => {
    const [cat, conns] = await Promise.all([commands.listProviderCatalog(), commands.listConnections(LOCAL_RUNNER)]);
    if (cat.status === "ok") set({ catalog: cat.data });
    if (conns.status === "ok") set({ connections: conns.data });
    set({ loaded: true });
  },
  add: async (provider, label, apiKey, baseUrl) =>
    apply(set, await commands.addConnection(LOCAL_RUNNER, provider, label, apiKey, baseUrl), "Add connection"),
  update: async (id, p) =>
    void apply(
      set,
      await commands.updateConnection(LOCAL_RUNNER, id, p.label, p.enabled, p.apiKey, p.baseUrl, p.models, p.claudeCloaking),
      "Update connection",
    ),
  remove: async (id) => void apply(set, await commands.removeConnection(LOCAL_RUNNER, id), "Remove connection"),
  move: async (id, dir) => void apply(set, await commands.moveConnection(LOCAL_RUNNER, id, dir), "Reorder"),
  test: async (id) => {
    const res = await commands.testConnection(LOCAL_RUNNER, id);
    if (res.status === "ok") return res.data;
    toast.error(`Test failed: ${res.error.message}`);
    return null;
  },
  connectOauth: async (provider, label) => apply(set, await commands.connectOauth(LOCAL_RUNNER, provider, label), "Connect"),
  reconnectOauth: async (connectionId) => apply(set, await commands.reconnectOauth(LOCAL_RUNNER, connectionId), "Reconnect"),
  beginOauthManual: async (provider) => {
    const res = await commands.beginOauthManual(provider);
    if (res.status === "ok") return res.data;
    toast.error(`Connect failed: ${res.error.message}`);
    return null;
  },
  completeOauthManual: async (provider, label, verifier, state, pasted, redirectUri) =>
    apply(set, await commands.completeOauthManual(LOCAL_RUNNER, provider, label, verifier, state, pasted, redirectUri), "Connect"),
  addFree: async (provider, label) => apply(set, await commands.addFreeConnection(LOCAL_RUNNER, provider, label), "Add connection"),
  startKiroDevice: async () => {
    const res = await commands.startKiroDeviceFlow(LOCAL_RUNNER);
    if (res.status === "ok") return res.data;
    toast.error(`${KIRO_SIGNIN_ACTION} failed: ${res.error.message}`);
    return null;
  },
  awaitKiroDevice: async (label, flowId) => apply(set, await commands.awaitKiroDeviceFlow(LOCAL_RUNNER, label, flowId), KIRO_SIGNIN_ACTION),
  importKiro: async (label) => apply(set, await commands.importKiroToken(LOCAL_RUNNER, label), KIRO_IMPORT_ACTION),
  startDeviceFlow: async (provider) => {
    const res = await commands.startDeviceFlow(LOCAL_RUNNER, provider);
    if (res.status === "ok") return res.data;
    toast.error(`Sign in failed: ${res.error.message}`);
    return null;
  },
  awaitDeviceFlow: async (provider, label, flowId) =>
    apply(set, await commands.awaitDeviceFlow(LOCAL_RUNNER, provider, label, flowId), "Sign in"),
}));
