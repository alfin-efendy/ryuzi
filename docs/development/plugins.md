# Plugin SDK

Ryuzi's extension points — model providers, CLI-agent runtimes, the Discord
gateway, and third-party integrations (GitHub, Notion, Slack, memory
backends, sandboxes, deploy platforms...) — are all **plugins**: one manifest
each, surfaced identically through `ryuzi plugins`, `GET /plugins`, and
Cockpit's sidebar + Apps → Catalog tab.

This guide covers the manifest format, how to author and install your own
plugin, and how the built-in fleet is organized. It documents what is
actually implemented on this branch — verify any command shown here still
matches `ryuzi --help` if you're reading this on a different revision.

---

## Two layers: manifest vs. `CorePlugin`

Every plugin has a **declarative** half and a **behavioral** half, owned by
two different crates:

- **`crates/plugin-sdk`** (`ryuzi-plugin-sdk`) owns the manifest contract:
  `PluginManifest` and its nested types, the category vocabulary
  (`categories::KNOWN`), and the `${...}` placeholder substitution grammar
  (`subst::resolve`). It depends on nothing but `serde`, `serde_json`,
  `toml`, and `thiserror` — no `ryuzi-core` dependency — so it's the small,
  stable contract a plugin author (or another crate) targets.
- **`crates/core/src/plugins/`** owns the binding: `CorePlugin { manifest,
  harness, gateway, connector, source }` pairs a manifest with the runtime
  capability it actually provides, `PluginHost` tracks every installed
  plugin, and `Registries` (`harness`/`gateway`/`connector`/`plugins`) is the
  composition root every host (`ryuzi` CLI, Cockpit's Tauri shell) builds at
  startup.

A manifest **on its own** can only ever produce a *connector* (an MCP-server
contributor) — that's what `declarative_plugin()`
(`crates/core/src/plugins/declarative.rs`) builds automatically whenever a
manifest declares `[[mcp]]` entries. A *harness* (an agent runtime) or a
*gateway* (a chat platform) requires actual Rust code, so those three
capabilities are hand-built-in: the native runtime and the `claude-code`
harness (beside their own modules, `harness::native` /
`harness::acp`), and the `discord` gateway
(`crates/core/src/plugins/builtin.rs`).

Manifests come from three places, merged in this order (first registration
for a given `id` wins — see `PluginHost::add`):

1. **Rust built-ins** — `native`, `claude-code`, `discord`, plus every model
   provider and CLI agent, generated from two existing static catalogs
   (`crates/core/src/plugins/providers.rs`, `runtimes_meta.rs`).
2. **The embedded integration catalog** — 24 TOML manifests baked into the
   binary via `include_str!`, at `crates/core/plugins/catalog/*.toml`
   (`crates/core/src/plugins/catalog.rs`).
3. **User plugins** — TOML manifests on disk at
   `~/.config/ryuzi/plugins/<id>/ryuzi-plugin.toml`
   (`crates/core::plugins::load_user_plugins`).

A real `ryuzi` process wires all of this at startup
(`crates/cli/src/main.rs`'s `build_registries`, mirrored by
`apps/cockpit/src-tauri/src/lib.rs`): register `native` unconditionally,
register `claude-code` if the ACP sidecar resolves, register `discord`,
then call `ryuzi_core::plugins::install_builtins` (providers, then CLI
agents, then the embedded catalog) and finally
`ryuzi_core::plugins::load_user_plugins`. Because this all runs once at
process startup, adding or editing a user plugin's manifest requires
restarting `ryuzi` (or the Cockpit app) to pick it up — there is no
hot-reload.

---

## Manifest reference

One plugin = one manifest, `ryuzi-plugin.toml` for catalog/user plugins.
Every field (`ryuzi_plugin_sdk::manifest::PluginManifest`):

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `contract` | integer | *(required)* | Contract version this manifest targets. `validate()` rejects `contract` greater than the loader's `CONTRACT_VERSION` (currently `1`). |
| `id` | string | *(required)* | Unique, kebab-case: must start with a lowercase letter or digit, then only lowercase letters, digits, or `-`. |
| `name` | string | *(required)* | Display name; must be non-empty. |
| `version` | string | `""` | Free-form; not validated. |
| `publisher` | string | `""` | `"ryuzi"` for first-party; a vendor or maintainer name otherwise. |
| `description` | string | `""` | Shown in `ryuzi plugins info`, `GET /plugins`, and the Cockpit catalog card. |
| `homepage` | string \| null | `None` | |
| `icon` | string \| null | `None` | A lucide icon name. Cockpit maps a small explicit set (`message-circle`, `terminal`, `cpu`, `globe`, `database`, `search`, `cloud`, `server`, `webhook`, `key`, `mail`, `bot`) and falls back to a generic puzzle icon for everything else — including brand-name icons like `github`, `slack`, or `figma`, since `lucide-react` dropped brand/logo icons (see `apps/cockpit/src/lib/plugin-icons.ts`). |
| `categories` | string[] | `[]` | See the vocabulary table below. Unknown labels are a non-fatal warning (`PluginManifest::warnings()`), never a validation error. |
| `verified` | bool | `false` | First-party/vendor-confirmed. Drives the `verified`/`experimental`/`community` status label (see below). |
| `experimental` | bool | `false` | Docs-only entry with no working `[[mcp]]` server — see [Enabling](#enabling-plugins). |
| `auth` | `[auth]` table \| null | `None` | See [Auth kinds](#auth-kinds-and-substitution). |
| `settings` | `[[settings]]` array | `[]` | Extra non-auth settings fields. |
| `mcp` | `[[mcp]]` array | `[]` | See [MCP server defs](#mcp-server-defs). |
| `skills` | `[[skills]]` array | `[]` | See [Skills bundling](#skills-bundling). |
| `menu` | `[menu]` table \| null | `None` | See [Menu contributions](#menu-contributions). |
| `provider` | `[provider]` table \| null | `None` | Model-provider capability block — see below. |
| `runtime` | `[runtime]` table \| null | `None` | CLI-agent capability block — see below. |

### `[auth]`

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `kind` | `"none"` \| `"api-key"` \| `"token"` \| `"oauth"` | *(required if `[auth]` present)* | |
| `setting` | string \| null | `None` | Settings-store key holding the secret (e.g. `plugin.github.token`). |
| `env` | string \| null | `None` | Fallback environment variable, read if `setting` is unset/empty. |
| `help_url` | string \| null | `None` | Where to obtain a credential; surfaced in error messages. |

### `[[settings]]`

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `key` | string | *(required)* | e.g. `plugin.github.host`. |
| `label` | string | *(required)* | |
| `help` | string | `""` | |
| `secret` | bool | `false` | Redacted in the settings UI/API the same way `auth.setting` is. |
| `required` | bool | `false` | See `ensure_auth`'s required-settings check below. |
| `kind` | `"string"` \| `"int"` \| `"bool"` | `"string"` | No catalog entry currently uses anything but the default. |

### `[[mcp]]`

See [MCP server defs](#mcp-server-defs) for the full field table.

### `[[skills]]`

| Field | Type | Default |
| --- | --- | --- |
| `name` | string | *(required)* |
| `description` | string | `""` |
| `path` | string | *(required)* — relative to the manifest's own directory |

### `[menu]`

| Field | Type | Default |
| --- | --- | --- |
| `section` | string | `"plugins"` |
| `label` | string \| null | `None` |

### `[provider]` (model-provider plugins)

| Field | Type | Default |
| --- | --- | --- |
| `format` | string | *(required)* — e.g. `"anthropic"`, `"openai"` |
| `base_url` | string \| null | `None` |
| `models` | `[{ id, label?, default? }]` | `[]` |

### `[runtime]` (CLI-agent plugins)

| Field | Type | Default |
| --- | --- | --- |
| `binary` | string \| null | `None` |
| `npm_package` | string \| null | `None` |
| `default_model` | string \| null | `None` |

`[provider]`/`[runtime]` are populated by the generated built-in provider
and CLI-agent plugins (`providers.rs`/`runtimes_meta.rs`); none of the 24
embedded catalog manifests use them — a third-party integration is a
connector, not a model provider or a CLI agent.

### Full annotated example

This is the real, shipping `crates/core/plugins/catalog/github.toml`,
quoted verbatim:

```toml
contract = 1
id = "github"
name = "GitHub"
version = "0.1.0"
publisher = "GitHub (official)"
description = "Repos, issues, and pull requests via GitHub's official remote MCP server (api.githubcopilot.com/mcp/). Wikis are not covered — no wiki toolset upstream. The previous `npx`-based reference server, published under the now-archived `modelcontextprotocol` reference-servers repo, is dead — do not use it. Auth accepts either GitHub's OAuth flow or a personal access token sent as a Bearer header — this manifest wires the token path so it works headlessly."
homepage = "https://github.com/github/github-mcp-server"
icon = "github"
categories = ["vcs", "issues"]
verified = true

[auth]
kind = "token"
setting = "plugin.github.token"
env = "GITHUB_PERSONAL_ACCESS_TOKEN"
help_url = "https://github.com/settings/tokens"

[[mcp]]
name = "github"
transport = "http"
url = "https://api.githubcopilot.com/mcp/"
headers = { Authorization = "Bearer ${auth}" }

[menu]
section = "plugins"
label = "GitHub"
```

Validating it:

```sh
$ cargo run -p ryuzi-cli -- plugins info github
id: github
name: GitHub
version: 0.1.0
publisher: GitHub (official)
description: Repos, issues, and pull requests via GitHub's official remote MCP server (api.githubcopilot.com/mcp/). ...
categories: vcs,issues
status: verified
capabilities: connector
enabled: disabled
auth: kind=Token setting=plugin.github.token env=GITHUB_PERSONAL_ACCESS_TOKEN help_url=https://github.com/settings/tokens
mcp: github transport=Http target=https://api.githubcopilot.com/mcp/
menu: section=plugins label=GitHub
```

(`status` is `verified` when `verified = true`; otherwise `experimental` when
`experimental = true`; otherwise `community`.)

---

## Category vocabulary

`ryuzi_plugin_sdk::categories::KNOWN` — 21 standard labels. Unrecognized
categories are a warning, not a validation error, so the vocabulary can grow
without breaking existing manifests.

| Category | Used for |
| --- | --- |
| `model-provider` | Every LLM API provider (paired with `api-key`/`oauth`/`free` below) |
| `api-key` | A model provider authenticated by API key (e.g. Anthropic, OpenAI) |
| `oauth` | A model provider authenticated via OAuth (e.g. `anthropic-oauth`, `openai-oauth`) |
| `free` | A free-tier model provider (e.g. `kiro`, `opencode-free`) |
| `runtime` | An agent runtime with a live harness (`native`, `claude-code`) |
| `cli-agent` | A CLI coding agent Cockpit can drive but doesn't run in-process (`claude`, `codex`, `gemini`, `opencode`, ...) |
| `chat-gateway` | A chat platform gateway (`discord`) |
| `vcs` | Source control / repos (`github`, `atlassian`) |
| `issues` | Issue trackers (`github`, `atlassian`, `linear`) |
| `docs` | Document stores (`notion`, `google-workspace`) |
| `wiki` | Wiki content (`atlassian`, `notion`) |
| `productivity` | Productivity suites (`notion`, `linear`, `google-workspace`) |
| `memory` | Agent memory backends (`mem0`, `zep`, `honcho`, `graphiti`, `cavemem`) |
| `knowledge-graph` | Graph-based memory (`graphiti`) |
| `search` | Web/local search (`brave-search`) |
| `design` | Design tools (`figma`) |
| `observability` | Monitoring and error tracking (`sentry`, `datadog`) |
| `sandbox` | Sandboxed code execution (`daytona`, `e2b`, `vercel-sandbox`) |
| `tunnel` | Network tunnels (`cloudflare`, `ngrok`) |
| `deploy` | Deployment platforms (`vercel`, `render`, `netlify`) |
| `communication` | Chat/messaging (`slack`, `telegram`) |

`model-provider`/`api-key`/`oauth`/`free` are provider-only labels generated
by `providers.rs`, not something a third-party integration manifest needs —
an integration's auth tier is described by `[auth].kind`, not by category.

---

## Auth kinds and substitution

### Auth kinds (`AuthKind`)

- `none` — no credential gate. `ensure_auth` never requires anything, but a
  manifest can still populate `setting`/`env` purely for `${auth}`
  substitution to draw from.
- `api-key` / `token` — a secret read from the settings store
  (`auth.setting`) with an environment-variable fallback (`auth.env`).
- `oauth` — for model providers this delegates to `llm_router`'s existing
  OAuth machinery (unchanged by the plugin SDK); for remote MCP servers v1
  has no broker — the manifest carries `help_url` only (e.g. `atlassian`,
  `notion`, `figma`, `slack`, `sentry`, `cloudflare`, `vercel` all ship with
  `kind = "oauth"` and no `setting`/`env`).

### Resolution order

`DeclarativeConnector::resolve_auth` (`crates/core/src/plugins/declarative.rs`)
tries, in order: the settings-store row named by `auth.setting` (if
present and non-empty), then the process environment variable named by
`auth.env` (if present and non-empty), then `None`. Neither the value nor
which source was used is ever logged.

### Substitution syntax

`ryuzi_plugin_sdk::subst::resolve` — a single linear scan, no regex — is
applied to every `[[mcp]]` `args`/`env` value/`headers` value/`url` at
session-attach time:

| Placeholder | Resolves to |
| --- | --- |
| `${auth}` | The resolved auth value (see above) |
| `${setting:KEY}` | The settings-store row named `KEY` |
| `${env:VAR}` | The process environment variable `VAR` |
| `$${` | Escapes to a literal `${` (not itself a placeholder) |

An unresolved placeholder (missing value, or a `${` with no closing `}`)
makes `resolve` return an error.

### `ensure_auth`

`DeclarativeConnector` implements the `Connector::ensure_auth` hook with two
checks:

1. If `auth.kind != none` and no value resolves (see above), it fails with
   `"configure {id}: see {help_url}"` (or `"...: missing credentials"` if no
   `help_url` is set).
2. Independently, every `[[settings]]` field with `required = true` must
   have a non-empty settings-store row, or it fails with `"configure {id}:
   missing required setting {key} — see {help_url}"`. This covers manifests
   whose `[[mcp]]` entry needs more than the auth credential alone — e.g.
   `honcho`'s `plugin.honcho.user` header (`X-Honcho-User-Name`) or
   `datadog`'s `plugin.datadog.app_key` (`DD-APPLICATION-KEY`).

**Current caveat:** `ensure_auth` is exercised by `crates/core/src/plugins/
declarative.rs`'s own tests, but session attach
(`control::lifecycle::attach_plugin_mcp_servers`) does **not** call it —
it only calls `is_enabled` and then `mcp_servers()` directly. If a
credential is missing, `mcp_servers()` itself fails (an unresolved `${auth}`
placeholder), and that failure is caught and logged via `tracing::warn!`,
silently skipping that plugin's servers for the session — nothing
surfaces to the CLI or Cockpit UI mid-session. Check `ryuzi plugins info
<id>`'s `auth:`/`setting:` lines (or the Cockpit plugin detail screen)
*before* enabling a plugin, rather than relying on a session-time error.

---

## MCP server defs

`McpServerDef`:

| Field | Type | Default |
| --- | --- | --- |
| `name` | string | *(required)*, unique within the manifest |
| `transport` | `"stdio"` \| `"http"` | *(required)* |
| `command` | string \| null | `None` — required if `transport = "stdio"` |
| `args` | string[] | `[]` |
| `env` | table (string → string) | `{}` |
| `url` | string \| null | `None` — required if `transport = "http"` |
| `headers` | table (string → string) | `{}` |

`validate()` rejects a `stdio` entry with no `command`, an `http` entry with
no `url`, and duplicate `name`s within one manifest.

Most of the 24 catalog entries are `http` (remote, hosted servers — no local
binary needed): `github`, `atlassian`, `notion`, `linear`, `slack`, `figma`,
`sentry`, `datadog`, `mem0`, `honcho`, `cloudflare`, `vercel`, `render`.
The `stdio` entries need a local binary already on `PATH` (or a running
Docker daemon):

| id | local requirement |
| --- | --- |
| `google-workspace` | `uvx` (the `uv` Python tool runner) |
| `telegram` | `uvx` |
| `brave-search` | `npx` |
| `netlify` | `npx` |
| `graphiti` | `uv` (`uv run main.py`) — plus its own LLM key and graph-DB env vars, set in `ryuzi`'s own process environment |
| `cavemem` | the `cavemem` npm CLI, one-time global install (`npm i -g cavemem && cavemem install`) |
| `daytona` | the `daytona` CLI, plus an active `daytona login` session |
| `e2b` | a local Docker daemon (`docker run ... mcp/e2b`) |

Three entries are docs-only (`experimental = true`, no `[[mcp]]` at all —
see [Enabling plugins](#enabling-plugins)): `ngrok`, `vercel-sandbox`, `zep`.

---

## Skills bundling

`SkillDef { name, description, path }` — `path` is relative to the
manifest's **own directory** on disk, and must be a *leaf* skill directory
(a `SKILL.md` file directly inside it, not a directory of subdirectories).

This only works for `PluginSource::User` plugins: `PluginHost::
enabled_skill_dirs` walks every **enabled** user-sourced plugin, resolves
each `skills[].path` against that plugin's own directory, and keeps only the
ones that exist on disk. Catalog and built-in plugins carry no on-disk base
directory, so their `skills[]` (if any) never surface this way even though
the manifest field exists on every plugin.

The resulting directories feed `SessionCtx::extra_skill_dirs`, which
`harness::native::skills::SkillRegistry::load_with` scans alongside the
worktree's own skill roots (`.ryuzi/skills`, `~/.config/ryuzi/skills`,
`~/.claude/skills`) — **worktree skills win name conflicts**: the registry
inserts worktree-sourced skills first and keeps the first name it sees, so a
plugin-bundled skill can never shadow one already defined in the worktree
or a user's global skills directory.

Example `ryuzi-plugin.toml` skill entry:

```toml
[[skills]]
name = "github-triage"
description = "Triage issues into labeled buckets"
path = "skills/github-triage"   # relative to this manifest's own directory
```

---

## Menu contributions

`[menu] { section = "plugins", label = "..." }` declares a plugin's Cockpit
sidebar contribution. In the current Cockpit build:

- The sidebar's "Plugins" section (`apps/cockpit/src/components/shell/
  Sidebar.tsx`) shows **every enabled plugin whose `source` is `catalog` or
  `user`** — it doesn't currently check for a `[menu]` block at all (the
  bulk `list_plugins`/`listPlugins` response doesn't carry `menu` data), so
  in practice every enabled integration gets a row. The row's label is
  always the manifest's `name`, not `menu.label` — `menu.label` is only
  exposed via `ryuzi plugins info`/`GET /plugins/{id}`/`plugin_detail`'s
  `menuLabel` field today, not consumed by the sidebar's own rendering.
- `menu.section` is not read for grouping either — there is one fixed
  "Plugins" header. `section` defaults to `"plugins"` and every catalog
  manifest uses that default.
- Every non-experimental catalog manifest is required to declare `[menu]`
  (enforced by a test in `crates/core/src/plugins/catalog.rs`), so treat it
  as required for any integration you author, even though today's sidebar
  render doesn't strictly depend on its contents.

---

## Installing user plugins

Drop a manifest at:

```
~/.config/ryuzi/plugins/<id>/ryuzi-plugin.toml
```

Any bundled skill directories go alongside it, referenced by a path
relative to that same directory (see [Skills bundling](#skills-bundling)).

- A missing `~/.config/ryuzi/plugins` directory is not an error — most
  installs have none.
- A manifest that fails to parse or fails `validate()` is logged via
  `tracing::warn!` and skipped — it never panics and never blocks the rest
  of the scan (a broken sibling plugin still loads).
- An `id` that collides with any built-in or embedded-catalog plugin loses:
  built-ins and the catalog are registered first, and `PluginHost::add`
  keeps the first registration for a given id.
- User plugins are disabled by default, exactly like catalog plugins (see
  below).
- Discovery happens once, at process startup — restart `ryuzi` (or the
  Cockpit app) after adding or editing a manifest.

---

## Enabling plugins

`PluginHost::is_enabled` (`crates/core/src/plugins/host.rs`) resolves
enablement by capability, in this priority order:

1. Unknown `id` → `false`.
2. Harness-capable (`native`, `claude-code`) → whether `id` is in the
   `enabled_runtimes` CSV setting.
3. Gateway-capable (`discord`) → whether `id` is in the `enabled_gateways`
   CSV setting.
4. `experimental = true` (docs-only: `ngrok`, `vercel-sandbox`, `zep`) →
   always `false` — there is no capability to enable, and this wins even if
   a stray `plugin.<id>.enabled = true` row exists.
5. No harness/gateway/connector capability at all (every model-provider and
   CLI-agent plugin) → always `true`.
6. Otherwise (every catalog/user integration with a connector) →
   `plugin.<id>.enabled == "true"`, defaulting to `false`.

Three equivalent ways to flip it:

```sh
# CLI
ryuzi plugins enable github
ryuzi plugins disable github
```

- **Cockpit**: the Apps view's **Catalog** tab (`Apps` → `Catalog` segmented
  tab) lists every catalog/user plugin with a category filter and an
  enable/disable `Switch` per card (disabled — greyed out — for
  `experimental` entries, since there's nothing to enable); the plugin
  detail screen (reached from the sidebar's Plugins section or the
  Catalog card's "Configure" button) has the same switch.
- **Settings keys directly**: `enabled_runtimes`/`enabled_gateways` are CSV
  strings (add/remove `id`, preserving order, no duplicates —
  `toggle_enabled`'s `toggle_csv` helper); everything else is
  `plugin.<id>.enabled` (`"true"`/`"false"`). Both `ryuzi plugins enable/
  disable` and Cockpit's `set_plugin_enabled` command delegate to the same
  `ryuzi_core::plugins::toggle_enabled` function, so the two surfaces can't
  drift.

```sh
$ ryuzi plugins list | grep github
github  GitHub  vcs,issues      disabled        verified
$ ryuzi plugins enable github
enabled github
$ ryuzi plugins list | grep github
github  GitHub  vcs,issues      enabled verified
```

Every settings key a plugin declares — its `[[settings]]` fields, its
`auth.setting` (registered as a synthetic secret `String` field), and the
always-present `plugin.<id>.enabled` (`Bool`) — is registered in a
process-wide table (`crate::plugins::plugin_field`) the first time that
plugin is added to a `PluginHost`. `settings::store::validate_setting` and
`settings::catalog::is_secret` both consult that table, so `plugin.*` keys
validate and redact the same way built-in settings do, without a
per-plugin case in either function.

---

## How built-ins map to plugins

| Plugin id(s) | Source | Capability |
| --- | --- | --- |
| `native` | `harness::native::native_plugin()` | harness (in-process agent loop) |
| `claude-code` | `harness::acp::claude_code_plugin[_with_resolver]()` | harness (ACP sidecar) |
| `discord` | `plugins::builtin::discord_plugin()` | gateway (feature-gated `serenity` factory) |
| `anthropic`, `openai`, `ollama`, ... (every `llm_router::registry::CATALOG` entry) | `plugins::providers::provider_plugins()` | manifest-only, `[provider]` block; category `model-provider` + `api-key`/`oauth`/`free` |
| `claude`, `codex`, `gemini`, `opencode`, ... (`runtimes::CATALOG`, minus `native`/`ollama`) | `plugins::runtimes_meta::cli_agent_plugins()` | manifest-only, `[runtime]` block; category `cli-agent` |
| `github`, `atlassian`, ... (24 entries) | Embedded TOML, `plugins::catalog::catalog_plugins()` | connector (via `declarative_plugin`) |

`native`/`ollama` are deliberately skipped from the CLI-agent catalog
mapping: `native` already has a richer harness-backed plugin under that id,
and `ollama` is already the `model-provider` plugin for the same underlying
service (Cockpit's "detect the binary, list installed models" view of it) —
mapping either again would just register a weaker duplicate that
`PluginHost::add` would then silently drop.

---

## Hook scripts

Plugins that need to observe or gate tool calls (rather than contribute an
MCP server) use hook scripts, not the manifest — see
`crates/core/src/harness/native/hooks.rs`. Scripts live at:

```
.ryuzi/hooks/<event>/
```

one executable per file, run in filename-sorted order, receiving the event
payload as JSON on stdin. `tool.before` is a gating event: a non-zero exit
denies the action and the script's stdout becomes the shown reason;
`tool.after` and `session.start` are observational only. A missing hooks
directory, or a script that isn't executable, is treated as "allow" — never
a hard failure.

---

## Authoring walkthrough

1. Write a manifest at `~/.config/ryuzi/plugins/<id>/ryuzi-plugin.toml`
   (see the [full example](#full-annotated-example) above for the shape).
2. Validate it:

   ```sh
   ryuzi plugins info <id>
   ```

   A parse or `validate()` failure is logged (`tracing::warn!`) and the
   plugin simply won't appear in `ryuzi plugins list`/`info` — rerun with
   `RUST_LOG=warn` if you need to see why.
3. Enable it:

   ```sh
   ryuzi plugins enable <id>
   ```

4. If it declares `[auth]`, set the credential first (a settings-store row
   under `auth.setting`, or the `auth.env` environment variable) — check
   with `ryuzi plugins info <id>` that `auth:` shows the key you expect.
5. Start (or resume) a session in a project. The plugin's `[[mcp]]` servers
   are attached at session start (`control::lifecycle::
   attach_plugin_mcp_servers`) alongside any DB-configured Apps servers — a
   DB-configured server of the same name always wins over a plugin's. Any
   bundled `[[skills]]` (leaf dirs with a `SKILL.md`) show up in the
   session's skill list, behind the worktree's own skills on a name clash.

Minimal example, verified end-to-end against the built binary:

```toml
# ~/.config/ryuzi/plugins/acme/ryuzi-plugin.toml
contract = 1
id = "acme-user"
name = "Acme User Plugin"
description = "Example user-authored plugin"
categories = ["productivity"]

[[mcp]]
name = "acme"
transport = "stdio"
command = "acme-mcp"
```

```sh
$ ryuzi plugins list | grep acme
acme-user       Acme User Plugin       productivity    disabled        community
$ ryuzi plugins info acme-user
id: acme-user
name: Acme User Plugin
...
capabilities: connector
enabled: disabled
mcp: acme transport=Stdio target=acme-mcp
```

---

## Catalog contribution guidelines

Adding an entry to `crates/core/plugins/catalog/*.toml` (and
`CATALOG_MANIFESTS` in `crates/core/src/plugins/catalog.rs`):

- **`verified = true` only for vendor-doc-confirmed rows** — the server
  command/URL was checked against the vendor's own current docs, not
  training-data recall or a plausible-looking guess. If you can't confirm
  it, ship `verified = false` (renders a "community" badge). If the vendor
  has no working MCP surface at all, ship `experimental = true` with **no**
  `[[mcp]]` block — a docs-only entry (see `ngrok`, `vercel-sandbox`, `zep`).
- **Record research provenance in the description**, not just in a design
  doc — the description field is what a user sees in `ryuzi plugins info`
  and the Cockpit catalog card, so name the exact server/binary, any
  required local tooling (`uv`/`docker`/a CLI), and any caveat (e.g.
  "wikis are not covered", "restricted to directory-published apps",
  "the exact launch subcommand is unconfirmed upstream").
- **Never ship an archived or dead package/endpoint.** MCP tooling moves
  fast — packages get archived and endpoints get retired. There's a
  regression test for this
  (`catalog::tests::no_manifest_references_an_archived_package_or_endpoint`)
  that fails the build if any manifest text matches a known-dead reference
  (e.g. the archived `@modelcontextprotocol/server-github`, the retired
  Atlassian `/v1/sse` endpoint). Add to that list if you discover another
  one.
- Every manifest needs: `contract = 1`, a kebab-case `id`, a `publisher`
  (vendor or community-maintainer name), `categories` drawn from
  `categories::KNOWN`, and — if it declares `[auth]` with a credential kind
  (anything but `none`) — a `help_url`.
- Secrets flow only through `${auth}` / `${setting:...}` / `${env:...}`
  substitution — never a literal token in the manifest.
- Run `cargo test -p ryuzi-core` after adding an entry: `catalog.rs`'s test
  module parses/validates every embedded manifest, checks id uniqueness
  (including against every built-in provider/CLI-agent/`native`/
  `claude-code`/`discord` id), requires every non-experimental entry to
  have both `[[mcp]]` and `[menu]`, and requires every `experimental`
  entry to have neither.
