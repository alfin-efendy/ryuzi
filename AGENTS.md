# Ryuzi Agent Instructions

Use this file as the root instruction set for the whole monorepo. Prefer the
nearest nested `AGENTS.md` if one is added later, but keep these root rules in
force unless a more specific file overrides them.

## 1. Commands First

Run commands from the repo root unless a command explicitly uses `--cwd` or
changes directory.

| Task | Command |
| --- | --- |
| Install all dev deps | `bun install` |
| First-time setup | `make setup` |
| Toolchain check | `make doctor` |
| Run Cockpit desktop app | `bun run cockpit:dev` or `make dev` |
| Build Cockpit desktop app | `bun run cockpit:build` or `make build` |
| Run current source CLI | `bun run ryuzi -- <args>` |
| Run JS/TS tests | `bun test` |
| Run Rust tests | `cargo test` |
| Run all tests | `make test-all` |
| Type-check TS workspaces | `bun run typecheck` |
| Lint | `bun run lint` |
| Format JS/TS and Rust | `bun run format && cargo fmt` or `make format` |
| Pre-commit JS gate | `make check` |
| Build CLI smoke binary | `bun build apps/cli/src/cli/index.ts --compile --target=bun-linux-x64 --outfile dist/ryuzi` |

Use Bun for JavaScript and TypeScript work:

- Use `bun <file>` instead of `node <file>` or `ts-node <file>`.
- Use `bun test` instead of Jest or Vitest.
- Use `bun install` instead of npm, yarn, or pnpm installs.
- Use `bun run <script>` instead of npm/yarn/pnpm script runners.
- Use `bunx <package> <command>` instead of `npx`.
- Bun loads `.env` automatically. Do not add `dotenv`.

Rust work uses Cargo:

- Use `cargo test`, `cargo fmt`, and `cargo clippy` when touching Rust crates.
- Do not replace existing Rust workspace conventions with JS tooling.

## 2. Reality Check Before Editing

Before changing code or docs, inspect the real project state:

1. Run `git status --short --branch`.
2. Read the relevant `package.json`, `Cargo.toml`, README, tests, and nearby
   source files.
3. Use `rg` or `rg --files` for search.
4. If the user mentions a branch that is not visible, run
   `git fetch --all --prune` and check again. Do not claim a branch exists if
   `git branch --all` and `git show-ref` do not show it.
5. Preserve unrelated worktree changes. Other agents may be working in the same
   repo or in Ryuzi worktrees.

The Rust CLI rewrite is active work in parallel. If a branch named
`feat/rust-cli-4*` or similar is available, inspect it before touching CLI
ownership, packaging, release, or docs. If it is not available locally or on
`origin`, say that clearly and make the smallest compatible change.

## 3. Monorepo Map

Ryuzi is a Bun workspaces monorepo plus a Cargo workspace.

| Path | Role | Primary tooling |
| --- | --- | --- |
| `apps/cli` | Current TypeScript CLI and Ink TUI | Bun, React, Ink |
| `apps/cockpit` | Tauri desktop app frontend | Bun, Vite, React, Tailwind v4 |
| `apps/cockpit/src-tauri` | Tauri shell and desktop commands | Cargo, Tauri 2 |
| `apps/mission-control` | Planned web app | Bun, future `@ryuzi/protocol` client |
| `apps/mobile` | Planned mobile app | Future mobile stack |
| `packages/core` | TS control plane, config, store, agents, gateways | Bun |
| `packages/protocol` | Runtime-free shared contracts | TypeScript only |
| `packages/ui` | Shared React UI system | React, Tailwind v4, shadcn, lucide |
| `crates/core` | Rust core library for durable control plane pieces | Cargo |
| `crates/hook` | `ryuzi-hook` Rust binary | Cargo |
| `npm/ryuzi` and `npm/platform/*` | Published npm launcher/packages | Bun/npm packaging |
| `assets/brand` | Brand assets | Do not regenerate casually |
| `docs` | Project documentation | Markdown |

Keep dependencies flowing inward:

- Apps can import packages.
- `packages/core` can import `packages/protocol`.
- `packages/protocol` must stay runtime-free and portable.
- `packages/ui` should not depend on app-specific state.
- Rust crates should expose clean boundaries for Tauri/CLI consumers instead of
  reaching into TS app internals.

## 4. Area Rules

### CLI

- Current source CLI entrypoint is `apps/cli/src/cli/index.ts`.
- Run it with `bun run ryuzi -- <args>`.
- CLI tests live under `apps/cli/test`.
- Keep terminal UI changes covered by Ink tests where possible.
- Do not assume the TypeScript CLI remains the long-term source of truth while
  the Rust CLI rewrite is in flight. Check active branches/worktrees first.
- Do not change npm launcher behavior in `npm/ryuzi` or `npm/platform/*`
  without checking packaging and release workflows.

### Cockpit Desktop

- Cockpit intentionally uses Vite inside the Tauri app. Do not apply a blanket
  "no Vite" rule here.
- Frontend entrypoint is `apps/cockpit/src/main.tsx`; app shell is
  `apps/cockpit/src/App.tsx`.
- Tauri/Rust code lives in `apps/cockpit/src-tauri`.
- Use `bun run cockpit:dev` for local desktop development.
- Use `bun run --cwd apps/cockpit build` for frontend build checks when the
  desktop shell is not needed.
- Keep generated bindings such as `apps/cockpit/src/bindings.ts` generated; do
  not hand-edit generated files unless explicitly required.

### Core and Protocol

- `packages/protocol` defines contracts: domain models, events, approval types,
  and `ControlPlaneApi`. Keep it free of Bun, Node, Discord, Tauri, filesystem,
  and database dependencies.
- `packages/core` owns runtime behavior: control plane, config, store, agents,
  gateways, observability, updates, and hooks.
- Prefer adding tests near existing tests under `packages/core/test` or
  `packages/protocol/test`.
- Do not duplicate protocol shapes inside apps. Import from `@ryuzi/protocol`.

### UI and Design System

- Reuse `@ryuzi/ui` components before creating app-local primitives.
- Shared UI lives in `packages/ui/src`; Cockpit-specific composition lives in
  `apps/cockpit/src/components` and `apps/cockpit/src/views`.
- Use lucide icons for icon buttons and controls when available.
- Follow existing Tailwind v4, shadcn base-nova, Geist, OKLCH token, and acrylic
  surface patterns.
- Keep cards for actual repeated items, modals, or framed tools. Do not nest
  cards inside cards.
- Prefer compact, work-focused UI for Cockpit. This is an operational desktop
  app, not a marketing landing page.
- Maintain stable dimensions for sidebars, title bars, toolbars, panels, tabs,
  counters, and list rows so dynamic content does not shift layout.

### Rust

- Cargo workspace members are declared in root `Cargo.toml`.
- `crates/core` is the Rust library boundary.
- `crates/hook` builds the `ryuzi-hook` binary.
- `apps/cockpit/src-tauri` depends on `ryuzi-core`.
- Keep async/runtime choices consistent with existing `tokio` usage.
- Run `cargo fmt` and targeted `cargo test -p <package>` when possible.

## 5. Testing Matrix

Choose the smallest meaningful verification set for the files you touched:

| Change | Minimum verification |
| --- | --- |
| Pure docs | Review rendered Markdown mentally; no test required |
| TS utility or protocol types | `bun test <path-or-pattern>` and `bun run typecheck` if types changed |
| `packages/core` runtime behavior | Targeted `bun test packages/core/test/...` |
| CLI behavior | Targeted `bun test apps/cli/test/...` plus `bun run ryuzi -- --help` when relevant |
| Cockpit React UI | Targeted `bun test apps/cockpit/src/...` plus `bun run --cwd apps/cockpit build` for broad UI changes |
| Shared UI | `bun test packages/ui` if applicable and `bun run typecheck` |
| Rust crate code | `cargo test -p <crate>` and `cargo fmt` |
| Cross-stack Tauri change | `bun run cockpit:build` or explain why it was not run |
| Release or npm packaging | Check `.github/workflows/*`, `scripts/npm/*`, `npm/*`, and run a smoke build |

CI uses Bun 1.3.14, `bun install --frozen-lockfile`, `bun run typecheck`,
`bun test`, `bunx biome ci .`, and a Bun compile smoke test for the CLI.

## 6. Good and Bad Examples

### Commands

Good:

```sh
bun test packages/core/test/config.test.ts
bun run typecheck
cargo test -p ryuzi-core
```

Bad:

```sh
npm test
npx biome ci .
node apps/cli/src/cli/index.ts
```

### Branch Claims

Good:

```sh
git fetch --all --prune
git branch --all --list "*rust-cli*"
git show-ref | rg "rust|cli"
```

Then report exactly what exists.

Bad:

```text
I found feat/rust-cli-4 and it changes the CLI.
```

when the branch is not present in refs.

### Cockpit Frontend

Good:

```tsx
import { Button } from "@ryuzi/ui";
import { Settings } from "lucide-react";
```

Use existing shell, sidebar, modal, segmented control, and token patterns.

Bad:

```tsx
<div className="rounded-3xl bg-purple-600 p-8">New dashboard</div>
```

This ignores the desktop design system and creates a one-off surface.

### Protocol

Good:

```ts
import type { CoreEvent } from "@ryuzi/protocol";
```

Bad:

```ts
type CoreEvent = { type: string; payload: any };
```

Duplicated contracts drift and cause router/client mismatches.

### Rust and TS Boundaries

Good:

```text
Expose a small Rust command/API, then call it from Tauri or the CLI wrapper.
```

Bad:

```text
Have Rust code depend on app-specific TS file layout or generated frontend
state.
```

## 7. Anti-Hallucination Rules

- Do not invent package scripts. Read `package.json` first.
- Do not invent Rust crates. Read `Cargo.toml` first.
- Do not assume planned apps are implemented just because their workspace
  folders exist.
- Do not assume Discord, Claude Code, Tauri sidecars, or update flows work
  without checking the relevant tests and docs.
- Do not hand-edit lockfiles unless dependency changes require it.
- Do not rewrite generated assets, icons, or brand files unless the task is
  explicitly about those assets.
- Do not convert Cockpit away from Vite or Tailwind because of generic Bun
  guidance. The current project intentionally uses Vite for Tauri.
- Do not hide verification gaps. If a test or build was not run, say why.

## 8. File Editing Discipline

- Keep changes scoped to the requested area.
- Preserve unrelated user or agent changes in the worktree.
- Prefer structured parsers/APIs over ad hoc string manipulation.
- Add comments only when they explain non-obvious behavior.
- Keep public contracts and user-visible behavior covered by tests.
- For large migrations, update docs and command examples in the same change.

## 9. Release and Packaging Notes

- Release automation lives in `.github/workflows`, `release-please-config.json`,
  `.release-please-manifest.json`, `scripts/npm`, `npm/ryuzi`, and
  `npm/platform/*`.
- The npm package exposes the `ryuzi` CLI. Packaging changes must preserve
  `ryuzi --help`, `ryuzi --version`, and install smoke behavior.
- Cockpit desktop artifacts are built by the `cockpit-desktop` workflow with
  Tauri on Linux, macOS, and Windows.

## 10. When Unsure

Prefer a short investigation over a confident guess:

```sh
rg --files
rg "symbolOrCommand"
git status --short --branch
git branch --all
bun test <target>
cargo test -p <crate>
```

Report findings using concrete paths, commands, and branch names.
