# @harness/ide (planned)

The `@harness/ide` desktop app (Plan 2) will consume `@harness/client` to talk to the router over the HTTP+WebSocket transport delivered in Plan 1 (`hr serve`). That transport exposes a `/rpc` endpoint for synchronous calls and a `/ws` WebSocket channel for live session events and approval round-trips, secured by either a loopback bearer token or OIDC JWT. The Electron shell, cockpit UI, and OIDC PKCE browser flow are Plan 2 work and are not built yet — this directory is reserved as the workspace.
