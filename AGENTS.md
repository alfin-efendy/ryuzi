# Ryuzi Agent Instructions

Use this file as the root instruction set for the whole monorepo. Prefer the
nearest nested `AGENTS.md` if one is added later, but keep these root rules in
force unless a more specific file overrides them.

The product is the Cockpit desktop app (a Tauri UI over the Rust engine in
`crates/core`) plus the headless runner daemon (`ryuzi`, built from
`crates/runner`). There is no interactive CLI product — the old TUI/CLI was
removed when the runner shipped. No TypeScript CLI, protocol package, or TS
core exist either.

## 1. Commands First

Run commands from the repo root unless a command explicitly uses `--cwd` or
changes directory.

| Task | Command |
| --- | --- |
| Install all dev deps | `bun install` |
| First-time setup | `make setup` |
| Toolchain check | `make doctor` |
| Run the ryuzi runner from source | `cargo run -p ryuzi-runner -- <args>` or `make runner ARGS="<args>"` |
| Run Cockpit desktop app | `bun run cockpit:dev` or `make dev` |
| Build Cockpit desktop app | `bun run cockpit:build` or `make build` |
| Run JS/TS tests | `bun test` |
| Run Rust tests | `cargo test -p ryuzi-core -p ryuzi-runner` (or `cargo test` for the whole workspace) |
| Run all tests | `make test-all` |
| Type-check TS workspaces | `bun run typecheck` |
| Lint JS/TS | `bun run lint` |
| Lint Rust | `cargo clippy -p ryuzi-core -p ryuzi-runner --all-targets -- -D warnings` |
| Format JS/TS and Rust | `bun run format && cargo fmt` or `make format` |
| Pre-commit JS gate | `make check` |
| Runner smoke test | `cargo build -p ryuzi-runner && ./target/debug/ryuzi --version && ./target/debug/ryuzi --help` |

Use Bun for JavaScript and TypeScript work:

- Use `bun <file>` instead of `node <file>` or `ts-node <file>`.
- Use `bun test` instead of Jest or Vitest.
- Use `bun install` instead of npm, yarn, or pnpm installs.
- Use `bun run <script>` instead of npm/yarn/pnpm script runners.
- Use `bunx <package> <command>` instead of `npx`.
- Bun loads `.env` automatically. Do not add `dotenv`.

Rust work uses Cargo:

- Use `cargo test`, `cargo fmt`, and `cargo clippy` when touching Rust crates.
- Keep clippy clean: CI fails on any warning in `ryuzi-core` and `ryuzi-runner`.
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

## 3. Monorepo Map

Ryuzi is a Cargo workspace (the product) plus a Bun workspaces monorepo (the
desktop UI and shared web UI).

| Path | Role | Primary tooling |
| --- | --- | --- |
| `crates/core` | `ryuzi-core` — engine: control plane, store (SQLite), gateways (Discord), harness, LLM router, scheduler, settings, telemetry, update, worktrees, plugin host (`src/plugins/`) + embedded integration catalog (`plugins/catalog/*.toml`) | Cargo |
| `crates/runner` | `ryuzi-runner` — the ryuzi headless runner daemon (binary: ryuzi) | Cargo |
| `crates/plugin-sdk` | `ryuzi-plugin-sdk` — declarative plugin contract: manifest types, category vocabulary, validation, placeholder substitution (no `ryuzi-core` dependency) | Cargo |
| `apps/cockpit` | Tauri desktop app frontend | Bun, Vite, React, Tailwind v4 |
| `apps/cockpit/src-tauri` | `ryuzi-cockpit` — Tauri shell and desktop commands, depends on `ryuzi-core` | Cargo, Tauri 2 |
| `apps/mission-control` | Planned web app (not implemented) | Bun |
| `apps/mobile` | Planned mobile app (not implemented) | Future mobile stack |
| `packages/ui` | `@ryuzi/ui` — shared React design system | React, Tailwind v4, shadcn, lucide |
| `npm/ryuzi` + `npm/platform/*` | npm launcher that spawns the prebuilt Rust binary | Node launcher, npm packaging |
| `scripts` | Release/packaging helpers (`scripts/npm`) and test helpers | Bun |
| `assets/brand` | Brand assets (canonical source; see its README) | Do not regenerate casually |
| `docs` | Project documentation (`docs/development/setup.md`, `docs/development/plugins.md`) | Markdown |

Keep dependencies flowing inward:

- `crates/runner` and `apps/cockpit/src-tauri` depend on `crates/core`; the
  engine never depends on its consumers.
- `apps/cockpit` imports `packages/ui`; `packages/ui` must not depend on
  app-specific state.
- The Cockpit frontend talks to Rust only through the generated bindings
  (`apps/cockpit/src/bindings.ts`), never by duplicating engine types by hand.

## 4. Area Rules

### Runner (Rust)

- Source lives in `crates/runner`; the binary is named `ryuzi`
  (`crates/runner/src/main.rs`, library in `src/lib.rs`). There is no TUI —
  the command surface is `setup`, `start`, `status`, `service`, `doctor`,
  `config`, plus `--version`/`--help` and the hidden `__daemon`.
- Run it with `cargo run -p ryuzi-runner -- <args>`.
- Tests are inline `#[cfg(test)]` modules plus integration tests in
  `crates/runner/tests` (assert_cmd; no insta snapshots).
- The runner's brand identity is text-only: glyph `r`, name `ryuzi`
  (see `assets/brand/README.md`).
- Do not change npm launcher behavior in `npm/ryuzi` or `npm/platform/*`
  without checking packaging and release workflows.

### Engine (crates/core)

- `ryuzi-core` owns runtime behavior: control plane, SQLite store, gateways,
  harness sessions, LLM router, scheduler, settings, telemetry, updates, and
  worktrees.
- Database access goes through `Store::with_conn`; do not hand-roll pool
  boilerplate at call sites.
- Prefer adding tests next to the code in `#[cfg(test)]` modules; integration
  tests live in `crates/core/tests`.

### Cockpit Desktop

- Cockpit intentionally uses Vite inside the Tauri app. Do not apply a blanket
  "no Vite" rule here.
- Frontend entrypoint is `apps/cockpit/src/main.tsx`; app shell is
  `apps/cockpit/src/App.tsx`.
- Tauri/Rust code lives in `apps/cockpit/src-tauri`. Keep `#[tauri::command]`
  functions thin: extract non-trivial logic into pure, unit-tested functions.
- Use `bun run cockpit:dev` for local desktop development.
- Use `bun run --cwd apps/cockpit build` for frontend build checks when the
  desktop shell is not needed.
- Keep generated bindings such as `apps/cockpit/src/bindings.ts` generated; do
  not hand-edit generated files unless explicitly required.

### UI and Design System

- Use `@ryuzi/ui` primitives — `Button`, `Input`, `Textarea`, `Combobox`,
  `FormField`, `Modal`, `ModalFooter`, `SettingsCard`, `MenuPanel`,
  `Segmented`, `Switch`, and friends. Do not write raw `<button>`, `<input>`,
  `<textarea>`, or `<select>` elements in Cockpit views or components.
  Value selection uses `Combobox`; `MenuPanel` is only for action menus and
  composer-anchored autocompletes.
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

### Rust Workspace

- Cargo workspace members are `crates/core`, `crates/runner`,
  `crates/plugin-sdk`, and `apps/cockpit/src-tauri` (declared in root
  `Cargo.toml`).
- Shared dependencies and lint levels live in `[workspace.dependencies]` and
  `[workspace.lints]` in the root `Cargo.toml`; members opt in with
  `workspace = true`. Add new shared deps there, not per-crate.
- Formatting is governed by `rustfmt.toml`; editor defaults by `.editorconfig`.
- Keep async/runtime choices consistent with existing `tokio` usage.
- Run `cargo fmt` and targeted `cargo test -p <package>` when possible.

## 5. Testing Matrix

Choose the smallest meaningful verification set for the files you touched:

| Change | Minimum verification |
| --- | --- |
| Pure docs | Review rendered Markdown mentally; no test required |
| Engine (`crates/core`) | `cargo test -p ryuzi-core` and `cargo fmt` |
| Runner (`crates/runner`) | `cargo test -p ryuzi-runner` plus `cargo run -p ryuzi-runner -- --help` when relevant |
| Tauri commands (`src-tauri`) | `cargo test -p ryuzi-cockpit` |
| Cockpit React UI | Targeted `bun test apps/cockpit/src/...` plus `bun run --cwd apps/cockpit build` for broad UI changes |
| Shared UI (`packages/ui`) | `bun test packages/ui` and `bun run typecheck` |
| Cross-stack Tauri change | `bun run cockpit:build` or explain why it was not run |
| Release or npm packaging | Check `.github/workflows/*`, `scripts/npm/*`, `npm/*`, and run the runner smoke test |

CI (`.github/workflows/ci.yml`) uses Bun 1.3.14 with
`bun install --frozen-lockfile`, then: `bunx biome ci .` + `shellcheck
install.sh`; `bun run typecheck` + `bun test`; `cargo fmt --check`,
`cargo clippy -p ryuzi-core -p ryuzi-runner --all-targets -- -D warnings`,
`cargo test -p ryuzi-core -p ryuzi-runner`, and a build + `--version`/`--help`
smoke of the `ryuzi` binary; Playwright e2e for Cockpit; and an osv-scanner
pass over both lockfiles.

## 6. Good and Bad Examples

### Commands

Good:

```sh
cargo test -p ryuzi-core
cargo run -p ryuzi-runner -- --help
bun test apps/cockpit/src/store.test.ts
bun run typecheck
```

Bad:

```sh
npm test
npx biome ci .
node scripts/anything.js
bun run ryuzi   # no such script — the runner is the Rust binary, not a Bun app
```

### Branch Claims

Good:

```sh
git fetch --all --prune
git branch --all --list "*feature*"
git show-ref | rg "feature"
```

Then report exactly what exists.

Bad:

```text
I found feat/some-branch and it changes the CLI.
```

when the branch is not present in refs.

### Cockpit Frontend

Good:

```tsx
import { Button, FormField, Input } from "@ryuzi/ui";
import { Settings } from "lucide-react";
```

Use existing shell, sidebar, modal, segmented control, and token patterns.

Bad:

```tsx
<button className="h-8 cursor-pointer rounded-md bg-primary px-3.5 text-[12.5px]">
  Save
</button>
```

Raw elements with hand-rolled Tailwind clusters duplicate the design system.

### Rust/TS Boundary

Good:

```ts
import { commands } from "@/bindings";
```

Expose a small `#[tauri::command]` in `src-tauri`, regenerate the bindings,
and call it through the generated `commands` object.

Bad:

```ts
type SessionInfo = { id: string; status: string };
```

Hand-duplicated engine shapes drift from the Rust types and cause
frontend/backend mismatches.

## 7. Anti-Hallucination Rules

- Do not invent package scripts. Read `package.json` first.
- Do not invent Rust crates. The workspace has exactly four members; read
  `Cargo.toml` first.
- Do not assume planned apps (`apps/mission-control`, `apps/mobile`) are
  implemented just because their workspace folders exist.
- Do not assume Discord or update flows work without checking the relevant
  tests and docs.
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
- Add comments only when they explain non-obvious behavior; write them as
  standalone documentation, not as references to code that no longer exists.
- Keep public contracts and user-visible behavior covered by tests.
- For large migrations, update docs and command examples in the same change.

## 9. Release and Packaging Notes

- Release automation lives in `.github/workflows`, `release-please-config.json`,
  `.release-please-manifest.json`, `scripts/npm`, `npm/ryuzi`, and
  `npm/platform/*`.
- The npm package `ryuzi` is a launcher: it resolves the matching
  `npm/platform/ryuzi-*` package and spawns the prebuilt Rust binary. Packaging
  changes must preserve `ryuzi --help`, `ryuzi --version`, and install smoke
  behavior.
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
