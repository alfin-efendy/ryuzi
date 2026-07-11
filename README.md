<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/wordmark-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/brand/wordmark-light.svg">
  <img src="assets/brand/wordmark-light.svg" alt="ryuzi" width="560">
</picture>

[![npm version](https://img.shields.io/npm/v/ryuzi.svg)](https://www.npmjs.com/package/ryuzi)
[![CI](https://github.com/alfin-efendy/ryuzi/actions/workflows/ci.yml/badge.svg)](https://github.com/alfin-efendy/ryuzi/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Gateway-agnostic **control plane** for Ryuzi's built-in coding agent — an in-process native harness that runs against your own model providers — driven from many clients (starting with Discord). `ryuzi` is the headless runner daemon: install it on any machine you want to run agent sessions on, then drive it from the Cockpit desktop app.

> Long-term: a **mission control** web app, an **IDE** desktop app, and a **mobile** app — all in this monorepo, all sharing one API/contract with the router.

## Prerequisites

| Need | Why |
| --- | --- |
| [`git`](https://git-scm.com/) | Sessions run inside git repositories. |
| A model provider | The native agent runs against your configured model providers (API key or OAuth) — set up in Cockpit's Models screen or the runner's `ryuzi setup` wizard. |
| [Bun](https://bun.sh) | Only for running **from source** (development). The installed `ryuzi` binary needs nothing else. |
| A Discord server | Only if you want to drive sessions from Discord (you must be able to add a bot to it). |

Check your environment any time with `ryuzi doctor`.

## Install

### Runner (headless daemon, macOS / Linux)

```sh
curl -fsSL https://raw.githubusercontent.com/alfin-efendy/ryuzi/main/install.sh | sh
```

Or via a package manager:

```sh
npm install -g ryuzi        # or: bun add -g ryuzi
```

The runner daemon is unix-only. On Windows, use the Cockpit desktop app below,
or install the runner inside WSL with the same curl command.

### Cockpit (desktop app)

Download the installer for your platform from the latest
[release](https://github.com/alfin-efendy/ryuzi/releases/latest) — runner
binaries and Cockpit installers ship on the same release (Cockpit keeps its
own version number, shown in the release notes):

| Platform | File |
| --- | --- |
| Windows (x64 / arm64) | `*-setup.exe` |
| macOS (Intel + Apple Silicon) | universal `.dmg` |
| Linux (deb/rpm, x64 / arm64) | `.deb` / `.rpm` |

Installers are currently unsigned: on macOS run
`xattr -d com.apple.quarantine /Applications/ryuzi.app` after installing;
on Windows click through the SmartScreen prompt. Verify downloads against
`cockpit-checksums.txt` on the release.

## Quick start

Verify your environment:

```bash
ryuzi doctor
# git:    OK 2.43.0
# settings: OK
# doctor: PASS
```

`doctor` prints `PASS` only when git and all required settings are present — otherwise `FAIL` with the missing pieces.

Seed the required settings, then run the daemon in the foreground:

```bash
ryuzi setup    # first-run wizard: prompts for each missing required setting
ryuzi start    # run the daemon in the foreground (Ctrl-C to stop)
```

`ryuzi start` serves the control API on `127.0.0.1:4483` (setting:
`control_port`) with a bearer token at the state dir's `control.token` —
drive sessions from Discord (once connected, see below) or from the Cockpit
desktop app pointed at this daemon.

To run it unattended instead of in the foreground, install it as a
systemd/launchd user service:

```bash
ryuzi service install    # install + start the background service
ryuzi status              # daemon state (pid, port, version)
```

## Connect Discord

Driving sessions from Discord needs a bot you create in the Discord Developer Portal. Expand the walkthrough:

<details>
<summary><b>Step-by-step: create &amp; connect a Discord bot</b></summary>

1. **Create an application.** Open the [Discord Developer Portal](https://discord.com/developers/applications) → **New Application**, name it, and create.
2. **Add a bot token.** Open **Bot** in the sidebar → **Reset Token** → copy it. This is your **`discord.token`** — keep it secret, treat it like a password.
3. **Know the bot permissions you'll need.** The bot posts messages and creates channels and threads, so it needs: **View Channels**, **Send Messages**, **Send Messages in Threads**, **Create Public Threads**, **Manage Channels** (it creates a channel per project and a thread per session), and **Read Message History**. You'll select these on the invite screen in step 7.
4. **Enable the Message Content intent.** Still on the **Bot** page, scroll to **Privileged Gateway Intents** and turn on **Message Content Intent**. The bot reads message text to respond — it connects but won't see your messages without this.
5. **Copy the Application ID.** Open **General Information** → copy **Application ID**. This is your **`discord.app_id`**.
6. **Copy your Server ID.** In Discord, enable **Settings → Advanced → Developer Mode**, then right-click your server icon → **Copy Server ID**. This is your **`discord.guild_id`**.
7. **Invite the bot to your server.** Open **OAuth2 → URL Generator**, select the scopes **`bot`** and **`applications.commands`**, then under **Bot Permissions** check the permissions from step 3. Open the generated URL and add the bot to your server.
8. **Enter the values.** Run `ryuzi setup`; the wizard prompts for `discord.token`, `discord.app_id`, and `discord.guild_id`. For headless automation you can use `ryuzi config set discord.token <value>` instead.
9. **(Optional) Restrict access.** Set `admin_role_ids` and/or `approver_role_ids` to comma-separated Discord role IDs to control who may administer the bot and who may approve tool use.

Run `ryuzi start` — the Discord gateway connects and you can drive sessions from your server.

</details>

## Configuration

Settings live in a local SQLite database at `~/.local/share/ryuzi/ryuzi.sqlite`. Most people set them through the `ryuzi setup` wizard, but `ryuzi config <get|set|list>` is available for headless automation.

| Setting | Default | Meaning |
| --- | --- | --- |
| `workdir_root` | *(required)* | Parent directory where your project repos live. |
| `default_model` | harness default | Default model for new projects. |
| `default_effort` | `medium` | Default reasoning effort for new projects. |
| `default_perm_mode` | `default` | Default approval mode: `default`, `acceptEdits`, or `bypassPermissions`. `bypassPermissions` selected via Discord `/connect` is allowed only for admins (see `admin_role_ids`). |
| `max_concurrent_runs` | `3` | Max simultaneous sessions. |
| `approval_timeout_ms` | `300000` | How long to wait for a tool approval. |
| `otel_endpoint` | *(blank)* | OpenTelemetry OTLP/HTTP endpoint (blank = console telemetry). |
| `admin_role_ids` | *(blank)* | Comma-separated Discord role IDs allowed to administer. **Blank = everyone is admin.** When set, only these roles may select `bypassPermissions` on `/connect`. |
| `approver_role_ids` | *(blank)* | Comma-separated role IDs allowed to approve tool use. **Blank = only the session starter may approve.** |
| `discord.token` | *(required for Discord)* | Bot token (secret). |
| `discord.app_id` | *(required for Discord)* | Application ID. |
| `discord.guild_id` | *(required for Discord)* | Server (guild) ID. |

## Runner command reference

| Command | What it does |
| --- | --- |
| `ryuzi setup` | First-run wizard: prompts for each missing required setting. |
| `ryuzi start` | Run the daemon in the foreground (Ctrl-C / SIGTERM to stop). |
| `ryuzi status` | Show daemon state (pid, port, version). |
| `ryuzi service <install\|uninstall\|status>` | Manage the systemd/launchd user service. |
| `ryuzi config <get\|set\|list>` | Read/write settings — headless automation. |
| `ryuzi doctor` | Check your environment (git, settings). |
| `ryuzi --help` (or `-h`) | Show help. |
| `ryuzi --version` (or `-v`) | Print the version. |

## Development (from source)

> **First time?** See [docs/development/setup.md](docs/development/setup.md) for the full toolchain setup on macOS, Linux, and Windows (Rust + MSVC + Windows SDK are needed for the Cockpit desktop app).

This is a Bun workspaces monorepo (Cockpit desktop app + shared UI) wrapping a Cargo workspace (the `ryuzi` runner + core engine — the product). From the repo root:

```bash
bun install                        # link workspaces (Cockpit, shared UI)
cargo run -p ryuzi-runner -- ...   # run the ryuzi runner from source (or: make runner ARGS="...")
bun test                           # run Cockpit/UI/script package tests
cargo test -p ryuzi-core -p ryuzi-runner   # run Rust tests
bun run typecheck                  # tsc --noEmit across the Bun workspaces
bun run lint                       # biome ci .
bun run format                     # biome check --write . && cargo fmt
```

### Layout

```
crates/
  core/              # ryuzi-core — engine: control plane, store, providers, agents, gateways, observability
  runner/            # ryuzi-runner — the ryuzi headless runner daemon
apps/
  cockpit/           # @ryuzi/cockpit — Tauri desktop app (thin UI over ryuzi-core)
  mission-control/   # @ryuzi/mission-control — web app (planned)
  mobile/            # @ryuzi/mobile — mobile app (planned)
packages/
  ui/                # @ryuzi/ui — shared UI components (Cockpit)
npm/
  ryuzi/             # npm launcher package — resolves and spawns the prebuilt Rust binary
  platform/          # per-platform binary packages (ryuzi-linux-x64, ryuzi-darwin-arm64, …)
assets/
  brand/             # canonical brand assets (wordmark, mark, favicon)
```

## Roadmap

Phase 1 ships the router: the `ryuzi` runner, the Discord gateway, and the native agent harness. Next come the **mission control** web app, the **IDE** desktop app, and the **mobile** app — all in this monorepo, all sharing one API/contract with the router.

## License

Released under the [MIT License](LICENSE).
