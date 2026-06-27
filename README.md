![Harness Router wordmark](assets/brand/wordmark.svg)

# harness-router (monorepo)

Gateway-agnostic **control plane** for running agent harnesses (starting with Claude Code) and driving them from many clients (starting with Discord).

> Long-term: a **mission control** web app, an **IDE** desktop app, and a **mobile** app — all in this monorepo, all sharing one API/contract with the router.

## Layout

```
apps/
  router/            # @harness/router — backend daemon + CLI + Discord gateway + Claude harness (Phase 1)
  mission-control/   # @harness/mission-control — web app (planned)
  ide/               # @harness/ide — desktop app (planned)
  mobile/            # @harness/mobile — mobile app (planned)
packages/
  protocol/          # @harness/protocol — shared contracts: domain models, events, ControlPlane API
docs/superpowers/    # specs & implementation plans
```

## Tooling

Bun workspaces (no extra monorepo tool yet). From the repo root:

```bash
bun install          # link workspaces
bun test             # run all package tests
bun run typecheck    # tsc --noEmit across the repo
bun run harness ...  # run the router CLI (alias of apps/router)
```

See `docs/superpowers/specs/` for the design and `docs/superpowers/plans/` for the milestone plans.
