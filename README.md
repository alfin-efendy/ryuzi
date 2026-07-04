<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/wordmark-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/brand/wordmark-light.svg">
  <img src="assets/brand/wordmark-light.svg" alt="ryuzi" width="560">
</picture>

[![npm version](https://img.shields.io/npm/v/ryuzi.svg)](https://www.npmjs.com/package/ryuzi)
[![CI](https://github.com/alfin-efendy/ryuzi/actions/workflows/ci.yml/badge.svg)](https://github.com/alfin-efendy/ryuzi/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

Gateway-agnostic **control plane** for running agent harnesses (starting with Claude Code) and driving them from many clients (starting with Discord). The CLI is `ryuzi` — *drive Claude Code from chat and terminal*.

> Long-term: a **mission control** web app, an **IDE** desktop app, and a **mobile** app — all in this monorepo, all sharing one API/contract with the router.

## Prerequisites

| Need | Why |
| --- | --- |
| [`git`](https://git-scm.com/) | Sessions run inside git repositories. |
| [`claude` CLI](https://docs.claude.com/en/docs/claude-code) | The Claude Code runtime. **Log in once on the host** — the runtime uses your host login. |
| [Bun](https://bun.sh) | Only for running **from source** (development). The installed `ryuzi` binary needs nothing else. |
| A Discord server | Only if you want to drive sessions from Discord (you must be able to add a bot to it). |

Check your environment any time with `ryuzi doctor`.

## Install

### CLI (macOS / Linux)

```sh
curl -fsSL https://raw.githubusercontent.com/alfin-efendy/ryuzi/main/install.sh | sh
```

Or via a package manager:

```sh
npm install -g ryuzi        # or: bun add -g ryuzi
brew install alfin-efendy/ryuzi/ryuzi
```

The CLI daemon is unix-only. On Windows, use the Cockpit desktop app below,
or install the CLI inside WSL with the same curl command.

### Cockpit (desktop app)

Download the installer for your platform from the latest
[`cockpit-v*` release](https://github.com/alfin-efendy/ryuzi/releases):

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
# claude: OK 2.1.191
# auth:   unknown (relies on host login)
# settings: OK
# doctor: PASS
```

`doctor` prints `PASS` only when git, claude, and all required settings are present — otherwise `FAIL` with the missing pieces.

Then launch the dashboard. **The first run starts an interactive setup wizard** — pick a gateway (Discord) and a runtime (Claude Code), then fill the required fields (at minimum `workdir_root`, the parent folder where your repos live):

```bash
ryuzi
```

## Smoke test in the terminal (no Discord)

The fastest way to confirm everything works — a one-shot session, no gateway required. Point it at any git repo and give it a prompt:

```bash
ryuzi run --dir ~/code/my-repo --prompt "List the files in this repo and summarize what it does"
```

| Flag | Meaning |
| --- | --- |
| `--dir <path>` | Git repository to run in (required). |
| `--prompt <text>` | What to ask the agent (required). |
| `--model <id>` | Override the model for this run. |
| `--effort <level>` | Reasoning effort (e.g. `medium`). |
| `--mode <mode>` | Permission mode: `default`, `acceptEdits`, or `bypassPermissions`. |

Output streams as the session runs and ends with `✓ done`.

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
8. **Enter the values.** Run `ryuzi`; the setup wizard prompts for `discord.token`, `discord.app_id`, and `discord.guild_id`. For headless automation you can use `ryuzi config set discord.token <value>` instead.
9. **(Optional) Restrict access.** Set `admin_role_ids` and/or `approver_role_ids` to comma-separated Discord role IDs to control who may administer the bot and who may approve tool use.

Run `ryuzi` again — the Discord gateway connects and you can drive sessions from your server.

</details>

## Configuration

Settings live in a local SQLite database at `~/.local/share/ryuzi/ryuzi.sqlite`. Most people set them through the `ryuzi` setup wizard, but `ryuzi config <get|set|list>` is available for headless automation.

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

## CLI reference

| Command | What it does |
| --- | --- |
| `ryuzi` | Open the dashboard; the first run launches the setup wizard. |
| `ryuzi doctor` | Check your environment (git, claude, settings). |
| `ryuzi run --dir <repo> --prompt <text> [--model x] [--effort y] [--mode m]` | One-shot session in a repo. |
| `ryuzi --help` (or `-h`) | Show help. |
| `ryuzi --version` (or `-v`) | Print the version. |

## Development (from source)

> **First time?** See [docs/development/setup.md](docs/development/setup.md) for the full toolchain setup on macOS, Linux, and Windows (Rust + MSVC + Windows SDK are needed for the Cockpit desktop app).

This is a Bun workspaces monorepo (Cockpit desktop app + shared UI) wrapping a Cargo workspace (the `ryuzi` CLI + core engine — the product). From the repo root:

```bash
bun install                     # link workspaces (Cockpit, shared UI)
cargo run -p ryuzi-cli -- ...   # run the ryuzi CLI from source (or: make cli ARGS="...")
bun test                        # run Cockpit/UI/script package tests
cargo test -p ryuzi-core -p ryuzi-cli   # run Rust tests
bun run typecheck               # tsc --noEmit across the Bun workspaces
bun run lint                    # biome ci .
bun run format                  # biome check --write . && cargo fmt
```

### Layout

```
crates/
  core/              # ryuzi-core — engine: control plane, store, providers, agents, gateways, observability
  cli/               # ryuzi-cli — the ryuzi CLI (the product)
apps/
  cockpit/           # @ryuzi/cockpit — Tauri desktop app (thin UI over ryuzi-core)
  mission-control/   # @ryuzi/mission-control — web app (planned)
  mobile/            # @ryuzi/mobile — mobile app (planned)
packages/
  ui/                # @ryuzi/ui — shared UI components (Cockpit)
```

## Roadmap

Phase 1 ships the router: the `ryuzi` CLI, the Discord gateway, and the Claude Code harness. Next come the **mission control** web app, the **IDE** desktop app, and the **mobile** app — all in this monorepo, all sharing one API/contract with the router.

## License

Released under the [MIT License](LICENSE).
