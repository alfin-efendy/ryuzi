# Plugin SDK

Ryuzi's extension points — model providers, CLI-agent runtimes, the Discord
gateway, and third-party integrations (GitHub, Notion, Slack, memory
backends, sandboxes, deploy platforms...) — are all **plugins**: one manifest
each, surfaced identically through `ryuzi plugins`, `GET /plugins`, and
Cockpit's Plugins hub.

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
3. **Skill packs** — TOML manifests the skills installer materialized at
   `~/.config/ryuzi/plugins/<id>/ryuzi-plugin.toml`
   (`crates/core::plugins::load_skill_pack_plugins`), each gated on a
   `.ryuzi-skill.json` provenance stamp.

A real `ryuzi` process wires all of this at startup
(`crates/cli/src/main.rs`'s `build_registries`, mirrored by
`apps/cockpit/src-tauri/src/lib.rs`): register `native` unconditionally,
register `claude-code` if the ACP sidecar resolves, register `discord`,
then call `ryuzi_core::plugins::install_builtins` (providers, then CLI
agents, then the embedded catalog) and finally
`ryuzi_core::plugins::load_skill_pack_plugins`. Because this all runs once at
process startup, installing or refreshing a skill pack requires restarting
`ryuzi` (or the Cockpit app) to pick it up — there is no hot-reload.

---

## Cockpit Plugins hub

Cockpit's plugin UI now lives under the dedicated **Plugins** screen
(`apps/cockpit/src/views/PluginsView.tsx`) plus the per-plugin detail screen
(`PluginDetailView.tsx`), not the old Apps-only catalog flow. The screen is
split into four tabs backed by thin Tauri commands:

- **Installed** — DB-backed MCP apps already added to the local machine
  (`apps_cmd.rs` / `useApps`).
- **Access** — which runtimes are allowed to call each installed app.
- **Browse** — a pure grid of the embedded **catalog** manifests with a
  category filter. The old MCP-registry browser (`registry_cmd.rs` /
  `registry_search`) has been removed entirely; MCP-server apps installed
  through it keep working as ordinary Apps rows. Hand-adding an MCP server
  stays available via **Add MCP server** (AddAppModal).
- **Skills** — Ryuzi-native skill-pack install / refresh / removal
  (`skills_cmd.rs`, backed by `crates/core/src/skills_install.rs`).

Catalog plugins are installed through the **Install wizard**: clicking
Install runs `begin_plugin_install`, which routes by the manifest's
`[auth]` kind — env var already set (zero input), token/api-key entry,
or OAuth. For OAuth plugins Cockpit performs RFC 8414 discovery against
`auth.resource` and, when the manifest declares
`dynamic-registration = true`, RFC 7591 Dynamic Client Registration —
falling back to a manual client-id form when registration is impossible.
The browser callback lands on a loopback server Cockpit runs at
`http://127.0.0.1:8976/plugin-oauth/<id>/callback`, so the happy path
needs no manual code paste (paste remains the fallback when the port is
taken or the callback times out). External-OAuth plugins (`auth.kind =
"oauth"` with neither an `auth.resource` nor a manifest `authorize-url` —
e.g. `google-workspace`) skip discovery, DCR, and the callback entirely:
the wizard only collects the client id, and the child server brokers
sign-in itself at first use. Required `[[settings]]` are collected before
the plugin is enabled.

---

## Manifest reference

One plugin = one manifest, `ryuzi-plugin.toml` for catalog plugins and
installer-materialized skill packs.
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
| `provider` | `[provider]` table \| null | `None` | Model-provider capability block — see below. |
| `runtime` | `[runtime]` table \| null | `None` | CLI-agent capability block — see below. |

### `[auth]`

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `kind` | `"none"` \| `"api-key"` \| `"token"` \| `"oauth"` | *(required if `[auth]` present)* | |
| `setting` | string \| null | `None` | Settings-store key holding the secret (e.g. `plugin.github.token`). |
| `env` | string \| null | `None` | Fallback environment variable, read if `setting` is unset/empty. |
| `help_url` | string \| null | `None` | Where to obtain a credential; surfaced in error messages. |
| `authorize-url` | string \| null | `None` | OAuth authorize endpoint, required for Cockpit's native plugin sign-in flow. |
| `token-url` | string \| null | `None` | OAuth token endpoint, required for Cockpit's native plugin sign-in flow. |
| `resource` | string \| null | `None` | Optional OAuth resource/audience parameter added to both authorize and token requests. |
| `scopes` | string[] | `[]` | Requested OAuth scopes. |
| `client-id-setting` | string \| null | `None` | Settings-store key that holds the OAuth client id. |
| `client-secret-setting` | string \| null | `None` | Optional settings-store key for the OAuth client secret. |
| `dynamic-registration` | bool | `false` | When `true`, the install wizard attempts RFC 7591 Dynamic Client Registration against the `registration_endpoint` discovered via RFC 8414; failure degrades to a manual client-id form. |
| `extra-authorize-params` | table (string → string) | `{}` | Extra query params appended to the authorize URL. |
| `extra-token-params` | table (string → string) | `{}` | Extra form fields appended to the token exchange request. |

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
- `oauth` — two distinct paths exist now:
  - **Model providers** still use `llm_router`'s provider-specific OAuth
    machinery.
  - **Plugin connectors** can use Cockpit's native plugin OAuth flow when the
    manifest declares `authorize-url`, `token-url`, and `client-id-setting`
    (plus any optional scopes/resource/extra params).

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

Session attach (`control::lifecycle::attach_plugin_mcp_servers`) calls
`ensure_auth` for every enabled, connector-capable plugin *before*
`mcp_servers()`. If `ensure_auth` fails (e.g. a missing credential), its
friendly, secret-free message is logged via `tracing::warn!` and that
plugin's servers are skipped for the session — a broken or unconfigured
plugin integration never fails session start, and the log message names
the plugin and what to configure, not just an unresolved-placeholder
parse error. Nothing surfaces to the CLI or Cockpit UI mid-session beyond
that log line, so check `ryuzi plugins info <id>`'s `auth:`/`setting:`
lines (or the Cockpit plugin detail screen) *before* enabling a plugin,
rather than relying on a session-time warning.

### Cockpit plugin OAuth sign-in

When a plugin detail screen sees `auth.kind = "oauth"`, Cockpit asks the Tauri
backend for a richer `PluginAuthInfo`:

- `oauthConnectAvailable = true` only when `resolve_plugin_oauth`
  (`plugins_cmd.rs`) can resolve an authorize URL, a token URL, and a client
  id. Resolution is **table-first**, not manifest-first: it reads this
  plugin's row in the same `plugin_oauth_clients` table the Install wizard
  populates via RFC 8414 discovery, RFC 7591 DCR, or the wizard's manual
  client-id form (see above) — falling back to the manifest's
  `authorize-url`/`token-url` for the endpoints, then to the saved value
  under `client-id-setting` for the client id, and, for external-OAuth
  plugins only, to the saved value under `auth.setting` as a last resort
  (`google-workspace`'s client-id setting key *is* its `auth.setting`). The
  manual client-id form (`set_plugin_oauth_client_id`) upserts straight into
  the `plugin_oauth_clients` row for non-external plugins — it deliberately
  never writes a `plugin.*` setting, since none of these manifests declare
  one.
- `begin_plugin_oauth` builds a PKCE authorize URL, opens it in the browser,
  emits `plugin-oauth-authorize-url-msg`, and shows the callback URL Cockpit
  expects (`http://127.0.0.1:8976/plugin-oauth/<id>/callback`).
- `complete_plugin_oauth` exchanges the pasted authorization code for a token
  and persists it in the store as a `PluginOauthToken`.
- `disconnect_plugin_oauth` deletes the saved token and returns the plugin to
  an unconfigured state.

Today this native sign-in path is available to plugin manifests whose
`auth.kind = "oauth"` resolves (table or manifest, per above) to full OAuth
metadata and a client id, regardless of MCP transport. Declarative HTTP MCP
entries use the stored token automatically as an `Authorization: Bearer ...`
header; other transports still need their manifests or provider-specific host
support to map the stored credential into the runtime process.

For declarative HTTP MCP entries, the connector checks token expiry before
injecting the bearer. If the token is due and a refresh token is available, it
refreshes against `token-url` and stores the new token before building the MCP
server spec. If refresh is impossible, or the provider returns a terminal OAuth
error such as `invalid_grant`, the token row is marked reconnect-required so
Cockpit can ask the user to sign in again.

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

This only works for `PluginSource::SkillPack` plugins: `PluginHost::
enabled_skill_dirs` walks every **enabled** skill-pack-sourced plugin, resolves
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

The separate Cockpit **Skills** tab drives the skills installer — now the
only path that produces plugin directories under `~/.config/ryuzi/plugins/`
(manifest-authored user plugins no longer load).
`crates/core/src/skills_install.rs` installs:

- **Single native skill repos** to `~/.config/ryuzi/skills/<skill-id>/`
- **Plugin skill packs** to `~/.config/ryuzi/plugins/<plugin-id>/`
- Then materializes each bundled skill into
  `~/.config/ryuzi/skills/<plugin-id>--<skill-id>/` so native sessions can
  discover the skills immediately without waiting for plugin manifest loading
  inside a separate process

Every materialized install gets a `.ryuzi-skill.json` provenance file so
refresh/remove can clean up both the plugin copy and the generated skill
directories without guessing.

---

## Installing skill packs

Hand-authored user plugins are no longer loaded — the only supported way
to get a plugin directory under `~/.config/ryuzi/plugins/` is the skills
installer (Cockpit's Plugins → Skills tab, or the same core path in
`crates/core/src/skills_install.rs`). `install_plugin_pack` writes:

- the pack repo to `~/.config/ryuzi/plugins/<plugin-id>/` (including its
  `ryuzi-plugin.toml`),
- a `.ryuzi-skill.json` provenance stamp **into that plugin directory**,
- and each bundled skill, materialized to
  `~/.config/ryuzi/skills/<plugin-id>--<skill-id>/` with its own
  provenance file.

`load_skill_pack_plugins` (called at startup by the CLI, `ryuzi daemon`,
and Cockpit) scans `~/.config/ryuzi/plugins/*/ryuzi-plugin.toml` and
registers a directory only when:

- it contains the `.ryuzi-skill.json` stamp, **or**
- (legacy packs installed before the stamp existed) the skills root holds
  a materialized skill whose provenance names the plugin id — in which
  case the stamp is healed into the plugin directory one time.

Directories matching neither (hand-authored manifests) are skipped with a
`tracing::warn!` — release-notes-worthy, by design.

Unchanged behaviors: a missing `~/.config/ryuzi/plugins` directory is not
an error; a manifest that fails to parse or fails `validate()` is logged
and skipped without blocking its siblings; an `id` colliding with any
built-in or embedded-catalog plugin loses (first registration wins); skill
packs are disabled by default like catalog plugins; discovery happens once
at process startup.

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
6. Otherwise (every catalog/skill-pack integration with a connector) →
   `plugin.<id>.enabled == "true"`, defaulting to `false`.

Three equivalent ways to flip it:

```sh
# CLI
ryuzi plugins enable github
ryuzi plugins disable github
```

- **Cockpit**: the dedicated **Plugins** screen's **Browse** tab lists every
  catalog plugin with a category filter; new installs go through the
  Install wizard, and each installed card keeps the enable/disable
  `Switch` (disabled — greyed out — for `experimental` entries, since
  there's nothing to enable); the plugin detail screen (reached from the
  sidebar's Plugins section or the Browse card's "Configure" button) has
  the same switch.
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

Hand-authored manifests under `~/.config/ryuzi/plugins/` are no longer
loaded (no installer provenance → skipped). To author a plugin:

1. **Catalog contribution** — add a TOML manifest under
   `crates/core/plugins/catalog/` and register it in `CATALOG_MANIFESTS`
   (see [Catalog contribution guidelines](#catalog-contribution-guidelines)).
2. **Skill pack** — publish a GitHub repo the skills installer
   understands (a `.codex-plugin/plugin.json` pack or a repo of leaf
   `SKILL.md` directories) and install it from Cockpit's Skills tab; the
   installer materializes `ryuzi-plugin.toml` and the provenance stamp
   for you.

   Minimal example — no `.codex-plugin/plugin.json`, just a `skills/`
   directory of leaf `SKILL.md` dirs:

   ```
   my-skills-repo/
   └── skills/
       └── code-review/
           └── SKILL.md
   ```

   Paste `owner/my-skills-repo` (or the full GitHub URL) into the Skills
   tab's **Install source** field. `discover_install_target` finds no
   `plugin.json` or top-level `SKILL.md`, scans `skills/*/SKILL.md`, and
   generates `~/.config/ryuzi/plugins/my-skills-repo/ryuzi-plugin.toml`
   with `id = "my-skills-repo"` and one `[[skills]]` entry
   (`path = "skills/code-review"`), plus the `.ryuzi-skill.json` provenance
   stamp — then materializes the skill to
   `~/.config/ryuzi/skills/my-skills-repo--code-review/`.

Then validate with `ryuzi plugins info <id>`, enable with
`ryuzi plugins enable <id>`, configure any `[auth]` credential, and start
a session — the plugin's `[[mcp]]` servers attach at session start
(`control::lifecycle::attach_plugin_mcp_servers`), with DB-configured
Apps servers of the same name winning, and bundled `[[skills]]` surfacing
behind worktree skills on a name clash.

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
  have `[[mcp]]`, and requires every `experimental` entry to have no
  `[[mcp]]`.
