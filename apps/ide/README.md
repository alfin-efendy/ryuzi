# @harness/ide — Electron Cockpit (milestone 2a)

`@harness/ide` is an Electron desktop app that connects to a locally running `hr serve` instance and provides a visual cockpit for managing Claude Code sessions.

## What's built (milestone 2a)

- **Auto-discovery:** on startup the main process reads `~/.local/share/harness-router/serve.json` (written by `hr serve`) and connects via `@harness/client` over HTTP+WebSocket.
- **Connection indicator:** the top bar shows a dot that reflects the live WebSocket state (`connecting` → `open` / `closed`).
- **Projects / sessions tree:** left pane lists all projects returned by the router and the sessions under each; click a session to activate it.
- **Live transcript pane:** right pane streams `CoreEvent` objects (`status`, `text`, `result`, `approval`) for the active session in real time.
- **Session lifecycle controls:** start, continue (send a prompt), stop, and end a session via the IPC bridge without leaving the cockpit.
- **Typed IPC bridge:** a `contextBridge`-exposed `window.harness` object provides all renderer↔main calls; the shared `ipc-contract.ts` keeps types consistent across both sides.
- **Zustand store:** renderer state (projects, sessions, transcripts, connection) is managed in a single zustand store that is updated by IPC events.

## What is NOT built yet

- **Interactive approvals UI** (milestone 2b) — approval events arrive in the transcript but there is no approve/reject button.
- **`connectProject` dialog** (milestone 2b) — no UI to link a workspace directory to a project.
- **Cloud OIDC connections** (milestone 2c) — only loopback bearer-token auth is wired; no PKCE browser flow or keychain storage.

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

Covered: `discoverLocalRouter`, IPC handlers, zustand store reducers, and `ProjectsSessionsTree` / `SessionTranscript` / `TopBar` component rendering.
