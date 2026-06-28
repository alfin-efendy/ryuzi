# @harness/ide — Electron Cockpit (milestone 2c)

`@harness/ide` is an Electron desktop app that connects to a locally running `hr serve` instance and provides a visual cockpit for managing Claude Code sessions.

## What's built (milestone 2c)

### Milestone 2a

- **Auto-discovery:** on startup the main process reads `~/.local/share/harness-router/serve.json` (written by `hr serve`) and connects via `@harness/client` over HTTP+WebSocket.
- **Connection indicator:** the top bar shows a dot that reflects the live WebSocket state (`connecting` → `open` / `closed`).
- **Projects / sessions tree:** left pane lists all projects returned by the router and the sessions under each; click a session to activate it.
- **Live transcript pane:** right pane streams `CoreEvent` objects (`status`, `text`, `result`, `approval`) for the active session in real time.
- **Session lifecycle controls:** start, continue (send a prompt), stop, and end a session via the IPC bridge without leaving the cockpit.
- **Typed IPC bridge:** a `contextBridge`-exposed `window.harness` object provides all renderer↔main calls; the shared `ipc-contract.ts` keeps types consistent across both sides.
- **Zustand store:** renderer state (projects, sessions, transcripts, connection) is managed in a single zustand store that is updated by IPC events.

### Milestone 2b

- **Interactive tool approvals:** when the router forwards a tool-approval request, an Allow/Deny card appears in the right rail with a live countdown timer; clicking Allow or Deny resolves the approval immediately and dismisses the card (timeout also auto-dismisses).
- **"+ Connect project" dialog:** a dialog in the left pane lets the user link a workspace to the router by entering a git URL or a local directory name; the IPC bridge calls `connectProject` which invokes `@harness/client` with the `ide` gateway and the current workspace ID.
- **Per-project "+ New session" dialog:** each project in the tree exposes a button that opens a dialog to start a new session, pre-scoped to that project.

### Milestone 2c — Cloud connections (OIDC + keychain + profiles)

This milestone completes Plan 2 (the cockpit app). The cockpit now manages a list of **connection profiles** — a synthetic local profile (the auto-discovered `hr serve` instance) plus any number of user-added remote router profiles — and lets the user switch the single active connection at any time.

**Connection management (main process):**

- **ConnectionsStore** — persists connection profiles to `~/.local/share/harness-router/connections.json`. Each profile carries a name, URL, and auth mode (`local` or `oidc`).
- **TokenStore + safeStorage** — OIDC access/refresh tokens are stored in the OS keychain via Electron's `safeStorage` API (AES-256 encryption backed by the platform secret service / Keychain / DPAPI). Tokens are loaded on startup, refreshed silently before expiry, and cleared on sign-out.
- **OIDC Authorization Code + PKCE (RFC 8252 loopback redirect)** — signing in to a remote router opens the system browser at the IdP's authorization URL. A short-lived local HTTP server on a random port receives the callback, exchanges the code for tokens via `openid-client`, and shuts down. No custom URI scheme is used.
- **ConnectionManager** — owns the active `@harness/client` instance; handles `select`, `add`, `remove`, `signIn`, and `signOut` commands, rebuilding the client whenever the active profile changes.
- **IPC surface** — six typed commands (`CONNECTIONS_CHANNEL`: `listConnections`, `addConnection`, `removeConnection`, `selectConnection`, `signIn`, `signOut`) plus a push event that streams `ConnectionSummary[]` snapshots to the renderer after every state change.

**Renderer:**

- **ConnectionsStore (Zustand)** — mirrors the main-process summary list and the active connection ID; updated via the IPC push event.
- **ConnectionsDialog** — lets the user view all profiles, switch the active connection, add a new remote (name + URL), sign in / sign out of remote profiles, and remove profiles.
- **TopBar trigger** — a "Connections" button in the top bar opens the dialog; the active connection name is displayed next to it.

**No-keyring limitation (Linux):** on a bare Linux system without a running secret-service (e.g. headless CI, plain WSL), `safeStorage` is unavailable. In that case the app falls back to session-only token storage: tokens are held in memory for the lifetime of the process and are never written to disk in plaintext. Re-authentication is required on every launch; no credentials are persisted.

**Test coverage:** unit tests mock the `OidcClient` seam and use a fake `Vault` (in-memory token store) — no display or IdP is required. The real OIDC browser flow (system browser ↔ IdP ↔ loopback callback) is verified by manual smoke test only; it cannot be automated in CI without a display and a live identity provider.

### Workspace tools — Phase 3 / milestone 3a

This milestone adds the read-only **Files** tab to a new resizable, tabbed **Right Panel** and relocates tool-approval cards inline into the session transcript.

**Files tab — worktree browser + CodeMirror viewer:**

- A lazy-loading directory tree lets you browse the active session's worktree.
- Clicking a file opens it in a read-only [CodeMirror](https://codemirror.net/) viewer with syntax highlighting.
- **Read-only — no editing or saving.** The viewer is intentionally non-editable.
- Backed by two new RPCs (`listDir` / `readFile`) exposed on `ControlPlaneApi` and implemented in `@harness/client`.
- **Path confinement:** every path is resolved via `realpathSync` and rejected if it escapes the worktree root; the `.git` directory is hidden from listings.
- **2 MB cap:** files larger than 2 MB are refused; binary files are surfaced as a base64/placeholder message rather than raw bytes.

**Inline approvals:**

- Tool-approval cards (Allow / Deny + countdown timer) now appear **inline in the session transcript**, directly after the event that triggered them.
- The separate right-rail `ApprovalsRail` component has been removed.

**Upcoming tabs (disabled placeholders today):**

- **Git review (3b)** — `gitStatus` / `getDiff` RPCs + read-only diff view.
- **Terminal (3c)** — PTY over a dedicated WebSocket channel + xterm.js renderer.

## What is NOT built yet

- **Git review tab** — planned for Phase 3b (read-only diff; no write operations).
- **Terminal tab** — planned for Phase 3c (PTY/xterm.js).
- **File editing/saving** — the Files tab is intentionally read-only; no `writeFile` RPC exists.
- **Multi-org / multi-active connections** — currently a single active connection at a time.
- **Remote settings editing** — reading and writing router config from the cockpit.

## Prerequisites

A local `hr serve` must be running before you start the IDE:

```sh
bun run hr serve
```

That command writes `~/.local/share/harness-router/serve.json` which the IDE reads on launch.

## Dev commands

```sh
# from apps/ide/
bun run dev     # build + watch + launch Electron (hot reload)
bun run build   # one-shot build to dist/
bun run start   # launch Electron against an existing dist/
```

> **Note:** a display (X11 / WSLg) is required to run the Electron window. Headless / CI environments should rely on the unit and component tests (`bun test`) rather than a visual smoke test.

## Testing

```sh
bun test   # runs the full monorepo suite including IDE unit + component tests
```

Covered: `discoverLocalRouter`, IPC handlers, zustand store reducers, `ProjectsSessionsTree` / `SessionTranscript` / `TopBar` component rendering, `ApprovalsRail` (card rendering + countdown + Allow/Deny interaction), `ConnectProjectDialog`, `NewSessionDialog`, `ConnectionsStore` (CRUD + persistence), `TokenStore` (save / load / clear / refresh with a fake `Vault`), loopback OIDC flow (`runLoopbackAuth` with a mock `OidcClient`), `ConnectionManager` (select / signIn / signOut / add / remove), `ConnectionsDialog` rendering + IPC call-arg assertions, and the 2c renderer Zustand store.

> **Note:** the GUI has not been headless-smoke-tested end-to-end; unit and component tests cover rendering and IPC call-arg assertions. The real OIDC browser flow requires a display and a live IdP and is verified by manual smoke test only.
