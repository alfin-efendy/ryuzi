import type { Page } from "@playwright/test";

/**
 * Fixtures mirror the generated types in src/bindings.ts (Project, Session).
 * Keep field names in sync when bindings regenerate.
 */
export const PROJECT = {
  projectId: "p-demo",
  name: "demo",
  workdir: "/tmp/demo",
  source: null,
  model: null,
  effort: null,
  permMode: "default",
  createdAt: 0,
};

export const SESSION = {
  sessionPk: "s-1",
  projectId: "p-demo",
  agentSessionId: null,
  worktreePath: null,
  branch: "main",
  title: null,
  status: "running",
  startedBy: null,
  createdAt: 0,
  lastActive: 0,
  resumeAttempts: 0,
  branchOwned: false,
  permMode: "default",
  kind: "project",
  speaker: null,
  agent: null,
  parentSessionPk: null,
};

/** A project-less chat session (kind "chat"), returned by start_chat_session. */
export const CHAT_SESSION = {
  ...SESSION,
  sessionPk: "c-1",
  projectId: null,
  branch: null,
  kind: "chat",
};

/** Tauri command → resolved value (Result-typed commands get the raw data). */
const FIXTURES: Record<string, unknown> = {
  list_projects: [PROJECT],
  list_sessions: [],
  list_messages: [],
  list_agents: [],
  refresh_agents: [],
  list_providers: [],
  list_runtimes: [],
  refresh_runtimes: [],
  list_gateways: [],
  probe_gateways: [],
  list_jobs: [],
  list_apps: [],
  get_setting: null,
  backdrop_capability: "none",
  system_accent_color: null,
  start_session: SESSION,
  start_chat_session: CHAT_SESSION,
};

/**
 * Installs a fake `window.__TAURI_INTERNALS__` before the app boots, so the
 * real `@tauri-apps/api` code path resolves against fixtures instead of a
 * missing Tauri bridge. `plugin:*` invokes (event listen, window show)
 * resolve to null. Every call is recorded on `window.__mockCalls`.
 */
export async function installMockIPC(page: Page, overrides: Record<string, unknown> = {}): Promise<void> {
  await page.addInitScript(
    (fixtures) => {
      const calls: Array<{ cmd: string; args: unknown }> = [];
      let cbId = 1;
      const w = window as unknown as Record<string, unknown>;
      w.__mockCalls = calls;
      w.__TAURI_INTERNALS__ = {
        metadata: {
          currentWindow: { label: "main" },
          currentWebview: { label: "main", windowLabel: "main" },
        },
        plugins: {},
        transformCallback: (cb: (payload: unknown) => void) => {
          const id = cbId++;
          Object.defineProperty(window, `_${id}`, { value: cb, configurable: true });
          return id;
        },
        invoke: (cmd: string, args: unknown) => {
          calls.push({ cmd, args });
          if (cmd.startsWith("plugin:")) return Promise.resolve(null);
          if (cmd in fixtures) return Promise.resolve(fixtures[cmd]);
          console.warn("[mock-ipc] unmocked command:", cmd);
          if (cmd.startsWith("list_") || cmd.startsWith("refresh_") || cmd.startsWith("probe_")) {
            return Promise.resolve([]);
          }
          return Promise.resolve(null);
        },
      };
    },
    { ...FIXTURES, ...overrides },
  );
}

export async function mockCalls(page: Page): Promise<Array<{ cmd: string; args: Record<string, unknown> | undefined }>> {
  return page.evaluate(() => (window as unknown as { __mockCalls: [] }).__mockCalls);
}
