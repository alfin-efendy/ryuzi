<picture>
  <source media="(prefers-color-scheme: dark)" srcset="assets/brand/wordmark-dark.svg">
  <source media="(prefers-color-scheme: light)" srcset="assets/brand/wordmark-light.svg">
  <img src="assets/brand/wordmark-light.svg" alt="Harness Router" width="560">
</picture>

# harness-router

[![npm version](https://img.shields.io/npm/v/hrctl.svg)](https://www.npmjs.com/package/hrctl)
[![CI](https://github.com/alfin-efendy/herness-router/actions/workflows/ci.yml/badge.svg)](https://github.com/alfin-efendy/herness-router/actions/workflows/ci.yml)
[![License: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](https://opensource.org/licenses/MIT)

Gateway-agnostic **control plane** for running agent harnesses (starting with Claude Code) and driving them from many clients (starting with Discord). The CLI is `hr` — *drive Claude Code from chat and terminal*.

> Long-term: a **mission control** web app, an **IDE** desktop app, and a **mobile** app — all in this monorepo, all sharing one API/contract with the router.

## Prerequisites

| Need | Why |
| --- | --- |
| [`git`](https://git-scm.com/) | Sessions run inside git repositories. |
| [`claude` CLI](https://docs.claude.com/en/docs/claude-code) | The Claude Code runtime. **Log in once on the host** — the runtime uses your host login. |
| [Bun](https://bun.sh) | Only for running **from source** (development). The installed `hr` binary needs nothing else. |
| A Discord server | Only if you want to drive sessions from Discord (you must be able to add a bot to it). |

Check your environment any time with `hr doctor`.

## Quick start

Install the CLI (the binary is `hr`):

```bash
npm i -g hrctl       # global install
# or try it without installing:
bunx hrctl --help
```

Verify your environment:

```bash
hr doctor
# git:      OK ...
# claude:   OK ...
# auth:     unknown (relies on host login)
# settings: ...
# doctor:   PASS | FAIL
```

Then launch the dashboard. **The first run starts an interactive setup wizard** — pick a gateway (Discord) and a runtime (Claude Code), then fill the required fields (at minimum `workdir_root`, the parent folder where your repos live):

```bash
hr
```

## Smoke test in the terminal (no Discord)

The fastest way to confirm everything works — a one-shot session, no gateway required. Point it at any git repo and give it a prompt:

```bash
hr run --dir ~/code/my-repo --prompt "List the files in this repo and summarize what it does"
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
3. **Copy the Application ID.** Open **General Information** → copy **Application ID**. This is your **`discord.app_id`**.
4. **Copy your Server ID.** In Discord, enable **Settings → Advanced → Developer Mode**, then right-click your server icon → **Copy Server ID**. This is your **`discord.guild_id`**.
5. **Invite the bot to your server.** Open **OAuth2 → URL Generator**, select the scopes **`bot`** and **`applications.commands`**, choose the permissions the bot needs, then open the generated URL and add the bot to your server.
6. **Enter the values.** Run `hr`; the setup wizard prompts for `discord.token`, `discord.app_id`, and `discord.guild_id`. For headless automation you can use `hr config set discord.token <value>` instead.
7. **(Optional) Restrict access.** Set `admin_role_ids` and/or `approver_role_ids` to comma-separated Discord role IDs to control who may administer the bot and who may approve tool use.

Run `hr` again — the Discord gateway connects and you can drive sessions from your server.

</details>

## Configuration

Settings live in a local SQLite database at `~/.local/share/harness-router/harness.sqlite`. Most people set them through the `hr` setup wizard, but `hr config <get|set|list>` is available for headless automation.

| Setting | Default | Meaning |
| --- | --- | --- |
| `workdir_root` | *(required)* | Parent directory where your project repos live. |
| `default_model` | harness default | Default model for new projects. |
| `default_effort` | `medium` | Default reasoning effort for new projects. |
| `default_perm_mode` | `default` | Default approval mode: `default`, `acceptEdits`, or `bypassPermissions`. |
| `max_concurrent_runs` | `3` | Max simultaneous sessions. |
| `approval_timeout_ms` | `300000` | How long to wait for a tool approval. |
| `otel_endpoint` | *(blank)* | OpenTelemetry OTLP/HTTP endpoint (blank = console telemetry). |
| `admin_role_ids` | *(blank)* | Comma-separated role IDs allowed to administer. |
| `approver_role_ids` | *(blank)* | Comma-separated role IDs allowed to approve tool use. |
| `discord.token` | *(required for Discord)* | Bot token (secret). |
| `discord.app_id` | *(required for Discord)* | Application ID. |
| `discord.guild_id` | *(required for Discord)* | Server (guild) ID. |

## CLI reference

| Command | What it does |
| --- | --- |
| `hr` | Open the dashboard; the first run launches the setup wizard. |
| `hr doctor` | Check your environment (git, claude, settings). |
| `hr run --dir <repo> --prompt <text> [--model x] [--effort y] [--mode m]` | One-shot session in a repo. |
| `hr --help` | Show help. |
| `hr --version` | Print the version. |

## Development (from source)

This is a Bun workspaces monorepo. From the repo root:

```bash
bun install          # link workspaces
bun run hr ...       # run the router CLI from source
bun test             # run all package tests
bun run typecheck    # tsc --noEmit across the repo
bun run lint         # biome ci .
bun run format       # biome check --write .
```

### Layout

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

See `docs/superpowers/specs/` for designs and `docs/superpowers/plans/` for milestone plans.

## Roadmap

Phase 1 ships the router: the `hr` CLI, the Discord gateway, and the Claude Code harness. Next come the **mission control** web app, the **IDE** desktop app, and the **mobile** app — all in this monorepo, all sharing one API/contract with the router.

## License

Released under the [MIT License](https://opensource.org/licenses/MIT).
