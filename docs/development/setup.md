# Pre-Development Setup

This guide covers everything needed to build the monorepo from source across all platforms. The repo contains two stacks:

- **Rust** (the `ryuzi` runner + engine in `crates/`, and the Cockpit desktop shell) — requires **Rust**; Cockpit additionally needs a C++ toolchain + **WebView**
- **JS/TS** (Cockpit frontend in `apps/cockpit`, shared UI in `packages/ui`) — requires **Bun**

If you only work on the runner/engine, you only need Rust. If you touch Cockpit (`apps/cockpit`), you need the full stack below.

---

## macOS

### 1. Xcode Command Line Tools

Provides `clang`, `git`, and the macOS SDK — required by Rust's linker.

```sh
xcode-select --install
```

### 2. Bun

```sh
curl -fsSL https://bun.sh/install | bash
```

### 3. Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

The default target (`aarch64-apple-darwin` on Apple Silicon, `x86_64-apple-darwin` on Intel) is correct — no extra steps needed.

### 4. Verify

```sh
make doctor
```

Expected output:

```
bun:   1.x.x
cargo: cargo 1.x.x
rustc: rustc 1.x.x
tauri: tauri-cli x.x.x
```

---

## Linux (Debian / Ubuntu)

### 1. System packages

Tauri needs WebKitGTK and several other libraries:

```sh
sudo apt update
sudo apt install -y \
  build-essential \
  curl \
  wget \
  file \
  libssl-dev \
  libgtk-3-dev \
  libayatana-appindicator3-dev \
  librsvg2-dev \
  libwebkit2gtk-4.1-dev \
  libxdo-dev \
  libsoup-3.0-dev \
  javascriptcoregtk-4.1
```

> **Fedora / RHEL:** Replace the `apt` block with the equivalent `dnf install` packages: `webkit2gtk4.1-devel`, `openssl-devel`, `gtk3-devel`, `librsvg2-devel`, `libappindicator-gtk3-devel`.

### 2. Bun

```sh
curl -fsSL https://bun.sh/install | bash
source "$HOME/.bashrc"   # or ~/.zshrc
```

### 3. Rust

```sh
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
source "$HOME/.cargo/env"
```

### 4. Verify

```sh
make doctor
```

---

## Windows

Windows requires the most setup because Rust needs the **MSVC** toolchain (not the GNU/MinGW one), which in turn needs **Visual Studio Build Tools** and the **Windows SDK**.

> **Important:** The default Rust installer on Windows may select the GNU toolchain. Follow the steps below exactly to avoid linker errors.

### 1. Git

Download and install from <https://git-scm.com/download/win>. Accept the default options.

### 2. Bun

Open **PowerShell** and run:

```powershell
powershell -c "irm bun.sh/install.ps1 | iex"
```

Restart the terminal after installation.

### 3. Visual Studio Build Tools (with C++ workload + Windows SDK)

Install **Visual Studio Build Tools** (or the full Visual Studio IDE):

```powershell
winget install Microsoft.VisualStudio.2022.BuildTools
```

When the installer opens, select the **"Desktop development with C++"** workload. This installs the MSVC compiler, linker (`link.exe`), and **Windows 11 SDK** in one step.

> **Already have Visual Studio installed?** Open the **Visual Studio Installer** → **Modify** → enable "Desktop development with C++" → ensure "Windows 11 SDK" is checked under Individual components → **Modify**.

Verify that `link.exe` is available. From a **Developer Command Prompt for VS**:

```cmd
where link.exe
```

It should print something like:
```
C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Tools\MSVC\14.x.x\bin\Hostx64\x64\link.exe
```

### 4. Rust (MSVC toolchain)

```powershell
winget install Rustlang.Rustup
```

Rustup's Windows installer defaults to the MSVC host. Confirm after installing:

```powershell
rustup show active-toolchain
# expected: stable-x86_64-pc-windows-msvc (default)
```

If it shows `windows-gnu` instead, switch it:

```powershell
rustup toolchain install stable-x86_64-pc-windows-msvc
rustup default stable-x86_64-pc-windows-msvc
```

> **Why not GNU?** The GNU toolchain (`x86_64-pc-windows-gnu`) requires MinGW binutils (`dlltool.exe`) which is a separate install and is not needed. Tauri's Windows support is built and tested against MSVC. Always use MSVC on Windows.

### 5. Verify

Open a **normal PowerShell** (not Developer Command Prompt — cargo finds the toolchain on its own):

```powershell
make doctor
```

Expected output:

```
bun:   1.x.x
cargo: cargo 1.x.x
rustc: rustc 1.x.x
tauri: tauri-cli x.x.x
```

---

## First-time setup (all platforms)

Once the toolchain is ready, from the repo root:

```sh
make setup   # bun install + cargo fetch
make cockpit # start Cockpit in dev mode (HMR)
```

`make setup` only needs to run once (and again after pulling major dependency changes).

---

## Engine daemon & control API

The engine (`ryuzi-core`) runs as a single background daemon process that
every surface talks to — there is no per-surface embedded engine anymore.

- **Single host.** The daemon (`ryuzi start` from the runner — a
  user-facing alias for the hidden `ryuzi __daemon` entry point, also used as
  the updater/canary respawn target — or Cockpit's hidden `--engine-daemon`
  mode) is the one process that owns the scheduler, the orchestrator loops,
  the gateways (Discord, etc.), and the `RouterServer` LLM-proxy endpoint.
- **Thin clients.** Cockpit attaches to an already-running daemon if it finds
  one, or auto-spawns `--engine-daemon` itself when none is running, then
  talks to it exclusively over the control API — it never opens the SQLite
  store or runs the scheduler/gateways in-process.
- **Control API.** Served on `127.0.0.1:${control_port:-4483}` (falls back to
  an ephemeral port if that one is taken). RPC calls and the SSE event stream
  require a bearer token read from `<state_dir>/control.token`, a file
  created `0600` at birth so it is never briefly world-readable.
- **Discovery.** `daemon.json` in the state dir carries the bound port (and
  other bring-up metadata) so clients can find a running daemon without
  guessing.
- **Singleton lock.** A `daemon.lock` file in the state dir enforces exactly
  one daemon per state dir — a second `__daemon` invocation exits immediately
  with an "already running" error instead of double-binding the store.

---

## Chat sessions

Sessions no longer require a project. A session's `kind` is `project`,
`chat`, `worker`, or `review`; `project` is the pre-existing kind (bound to a
project workdir), and `chat` is the project-less, chat-first kind added in
this phase (`worker`/`review` are schema-only so far, reserved for a later
phase's async delegation).

- **No project, no worktree.** A chat session's `project_id` is `None` and it
  never gets a git worktree. It runs in a scratch directory at
  `state_dir()/chat/<session_pk>`, created on first use.
- **Global memory only.** The native runtime's persistent-memory tool always
  builds global-scope memory; project-scope memory stays unavailable without
  a bound project. A chat session can read/write global memory, just not
  project memory.
- **Start points.** Cockpit Home starts a chat session automatically when no
  project is attached (the project picker is an optional "attach a project"
  control, not a hard requirement). Chat sessions also get their own bucket
  in the sidebar, above the project tree. A Discord DM starts a chat session
  too — no `/connect` step needed first.

---

## Background rail & async delegation

The daemon owns a durable **background rail** (`background_events` table,
migration #23) so work that finishes outside a chat's current turn can still
find its way back into that chat, even across a daemon restart.

- **Rail delivery is idle-only.** A producer (async delegation, a scheduled
  job, etc.) enqueues a row targeting a `session_pk`. The drainer only
  delivers a row when that session is actually idle — it injects the payload
  as a **new user turn** via `continue_session_with_prompt`. It never
  interrupts a turn in progress. Delivered rows are kept as history; rows are
  never lost to a daemon restart because the queue lives in SQLite, not
  memory.
- **`task` with `background: true`.** The native `task` tool accepts a
  `background: true` flag: the child subtask runs as a detached in-process
  worker instead of blocking the parent turn, and the parent gets an
  immediate "dispatched" acknowledgement. Capacity is the same shared
  `max_concurrent_runs` setting (`n`, default 3) that already caps
  orchestrator fan-out and sync task batches — at capacity a background
  dispatch is **rejected with a fallback-to-sync note**, not queued, so
  callers never get a delegation stuck waiting behind someone else's slot.
- **Completion re-entry.** When a background child finishes, its report is
  summary-budgeted (head/tail-trimmed to a token-derived character cap; an
  over-cap report spills the full text to a file under
  `state_dir()/chat/<session_pk>/delegations/` and the summary's footer
  points at it, so a `read`-paging call recovers the full result), wrapped in
  Hermes' verbatim `[ASYNC DELEGATION COMPLETE — {id}]` block, and enqueued
  to the rail (`kind: "delegation"`) targeting the parent session.
- **Session-end cleanup.** Ending a session cancels any of its still-running
  background workers and deletes its pending (undelivered) rail rows, so a
  chat that ends mid-delegation never has an orphaned turn reappear in a
  later, unrelated session.
- **Cron via the rail.** Scheduled-job output no longer notifies out of band
  — it delivers through the same rail (`kind: "job"`) to the job's home
  session. Jobs also gained an optional per-job `model_override`, letting a
  job start its session on a specific model instead of the project/agent
  default.

---

## Auxiliary model settings

Three secondary (non-primary-turn) LLM calls each read their own optional
model override from a raw settings key, so you can route them to a cheaper
or faster model than the session's main model:

| Setting key | Routes | Falls back to |
| --- | --- | --- |
| `auxiliary.title.model` | Session-title generation | The session's model |
| `auxiliary.compaction.model` | Context-compaction summarization | The session's model |
| `auxiliary.decompose.model` | Orchestrator goal-decompose | The configured default model |

Each key is unset by default (fallback applies). They are plain key-value
settings with no dedicated UI yet — set them out-of-band via the `set_setting`
RPC/Tauri command (`{ key: "auxiliary.title.model", value: "<model-id>" }`) or
directly in the `settings` table.

---

## Group-chat orchestration

A goal can fan out into a tree of tracked subtasks, each run by its own
worker session, with progress and outcomes reported back into the
originating chat as labeled bubbles (`crates/core/src/orch.rs`).

- **Starting a run.** Toggle "Orchestrate" on the Home composer (only
  enabled once a project is attached — orchestration always needs a project
  to scope the goal to) or type `/orchestrate <goal>`; both route to
  `orch_submit` instead of a normal chat turn
  (`apps/cockpit/src/views/HomeView.tsx`). With `decompose: true` (the
  toggle's default) an LLM plans the subtasks in the background while the
  root sits in `decomposing`; `auxiliary.decompose.model` (above) can route
  that planning call to a cheaper model than the session's own.
- **Worker sessions, hidden from the sidebar.** Each subtask runs as its own
  `kind='worker'` session bound to the root's project. Worker (and `review`)
  sessions never appear in a sidebar bucket — `sessionsForProject` and
  `chatSessions` (`apps/cockpit/src/lib/sidebar.ts`) only ever surface
  `kind === "project"` or `kind === "chat"` rows. The only way to reach a
  worker is the task strip's chip
  (`apps/cockpit/src/components/session/TaskStrip.tsx`), a pinned row of
  per-subtask pills above the home chat's transcript while a root is live.
  Clicking a chip drills into that worker's full `SessionView` (every tool
  call it made); the strip clears once the root settles.
- **Speaker bubbles.** A worker's start and its final report post into the
  home chat as speaker-labeled bubbles (`messages.speaker`, migration #29);
  distinct workers render with distinct agent-colored labels
  (`apps/cockpit/src/lib/agent-color.ts`). Once every child is done, the
  root's judge session posts its verdict as an `"orchestrator"` bubble AND
  re-enters the home chat as a new turn over the background rail
  (`kind='orch'`, wrapped in an `[ORCHESTRATION COMPLETE — …]` marker) —
  delivery is idle-only, like every other rail row, never mid-turn.
- **Steering.** Typing an ordinary message into a home chat while its
  orchestration is live does not start a normal turn: `orch_steer`
  (`crates/core/src/orch.rs::note_steer`) intercepts it first. `cancel` or
  `/cancel` (case-insensitive) cancels the whole tree; anything else is
  appended to the root's `steer_note` column as accumulating guidance the
  judge reads on its next pass. `orch_steer` reports back `noOrchestration`
  when no live root is bound to that session, so ordinary chats fall
  through to a normal turn unaffected.
- **Block-for-human.** A worker can call the native `orch_block` tool
  (visible only inside worker sessions) to pause on a blocking question.
  The task's status flips to `blocked` and Cockpit renders a `BlockCard` in
  the home chat; the human's answer goes back through
  `ControlPlane::answer_orch_block` (Tauri command `orchAnswerBlock`),
  enqueued on the background rail as a `kind='unblock'` row and delivered as
  a clean new user turn once the worker session is idle again — never a
  mid-turn splice.
- **Retry + circuit breaker.** A worker run that fails is requeued (`todo`)
  for another attempt rather than failing the root immediately.
  `MAX_TASK_RETRIES` (`crates/core/src/orch.rs`, currently `2`) caps
  consecutive failures per child — it is a Rust constant, not a runtime
  setting. Once a child exceeds it the breaker trips (`gave_up=1`,
  `status='failed'`) and, once every child under a root has given up, the
  root itself fails — delivered into the home chat through the same
  `deliver_outcome` path a successful verdict uses.
- **Concurrency cap.** Orchestrator fan-out shares the same
  `max_concurrent_runs` setting (default `3`) described under Background
  rail & async delegation above — the same knob also caps sync task batches
  and background `task` delegations, so a busy daemon can't be
  over-subscribed by any one of the three callers.

**Known v1 limitation — the Home double turn.** Orchestrating from a fresh
Home submission (Orchestrate toggle) creates the home chat by posting the
goal as its first user message, which — like starting any new chat — runs
one ordinary chat-agent turn on that same goal, *in parallel* with the
worker bubbles the orchestration itself produces
(`apps/cockpit/src/views/HomeView.tsx`'s `send()` calls `startChat(goal,
…)` and then `startOrchestration(goal, true)`). This is harmless: the home
chat's agent turn runs in its own scratch directory (see Chat sessions
above), never the real project worktree, and does not affect the
orchestration's actual work. A planned fast-follow — an "empty-home-chat"
primitive that creates the home chat with the goal as a display-only
opening message and no chat-agent turn — will remove the redundant turn.
Seeing duplicate agent activity on a fresh orchestrated Home submission is
expected v1 behavior, not a bug.

---

## Troubleshooting

### `bun: command not found: tauri`

The JS dependencies are not installed. Run `bun install` from the repo root.

### `error calling dlltool 'dlltool.exe': program not found` (Windows)

Your Rust default is the GNU toolchain. Switch to MSVC:

```powershell
rustup default stable-x86_64-pc-windows-msvc
```

### `linker 'link.exe' not found` (Windows)

Visual Studio Build Tools are missing or the C++ workload was not selected. Rerun the VS Installer and enable **"Desktop development with C++"**.

### `cannot open input file 'kernel32.lib'` (Windows)

The **Windows SDK** is not installed. Open the VS Installer → Modify → Individual components → search for **"Windows 11 SDK"** → check it → Modify.

### WebKitGTK not found (Linux)

Run the system package install step again with `sudo apt install libwebkit2gtk-4.1-dev`.
