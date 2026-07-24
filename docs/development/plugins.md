# Plugin SDK

Ryuzi's extension points — model providers, chat gateways, and
third-party integrations (GitHub, Notion, Slack, memory backends,
sandboxes, deploy platforms...) — are all **plugins**: one manifest
each, surfaced identically through the daemon's `list_plugins` RPC and
Cockpit's Plugins hub. There is no CLI surface for plugin management —
Cockpit (backed by the daemon's RPCs) is the only management surface.

This guide covers the manifest format, how to author and install your own
plugin, and how the built-in fleet is organized. It documents what is
actually implemented on this branch — verify any command shown here still
matches the daemon's current RPC surface if you're reading this on a
different revision.

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
  composition root the daemon builds at startup inside
  `ryuzi_core::daemon::build_daemon` — used by both the runner (`ryuzi
  start`) and Cockpit's `--engine-daemon` mode.

A manifest **on its own** can only ever produce a *connector* (an MCP-server
contributor) — that's what `declarative_plugin()`
(`crates/core/src/plugins/declarative.rs`) builds automatically whenever a
manifest declares `[[mcp]]` entries. A *harness* (the agent loop) requires
actual Rust code, so it is hand-built-in: the native agent harness
(`harness::native`). *Gateways* (chat platforms) now ship as signed WASM
component bundles discovered off-disk and driven through the generic host
gateway bridge (`crates/core/src/plugins/wasm_gateway_bridge.rs`) — the
first-party Discord gateway lives at `plugins/discord`; there is no native
(in-process) gateway plugin. A *connector* can, separately, ALSO ship as a
signed WASM component bundle instead of (or, for `github`/`atlassian`,
alongside) a declarative manifest — a distinct, additive mechanism covered
in [WASM component bundles](#wasm-component-bundles) below.

Manifests come from three places, merged in this order (first registration
for a given `id` wins — see `PluginHost::add`):

1. **Rust built-ins** — `native`, plus every model provider,
   generated from the static provider catalog
   (`crates/core/src/plugins/providers.rs`).
2. **The embedded integration catalog** — 24 TOML manifests baked into the
   binary via `include_str!`, at `crates/core/plugins/catalog/*.toml`
   (`crates/core/src/plugins/catalog.rs`) — still including `github` and
   `atlassian` as declarative, token-authenticated connectors; this list is
   unrelated to the signed WASM component bundles under `plugins/<id>` (see
   [WASM component bundles](#wasm-component-bundles) below).
3. **Skill packs** — TOML manifests the skills installer materialized at
   `~/.config/ryuzi/plugins/<id>/ryuzi-plugin.toml`
   (`crates/core::plugins::load_skill_pack_plugins`), each gated on a
   `.ryuzi-skill.json` provenance stamp.

The daemon wires all of this at startup inside `ryuzi_core::daemon::build_daemon`
— the one composition root used by both the runner's `ryuzi start` (via
`ryuzi __daemon`) and Cockpit's hidden `--engine-daemon` mode, since Cockpit
is a thin client that attaches to (or spawns) a daemon rather than building
its own registries: register `native` unconditionally, then call
`ryuzi_core::plugins::install_builtins` (providers, then the embedded
catalog) and finally `ryuzi_core::plugins::load_skill_pack_plugins`. Gateway
WASM component bundles are discovered off-disk and wired separately in the
same function (`build_wasm_gateways`); the native `enabled_gateways` +
`extra_gateway_factories` seam remains as generic infrastructure but has no
built-in gateway today. Because plugin registration runs once at daemon
startup, installing or refreshing a skill pack requires restarting the
daemon to pick it up — there is no hot-reload.

---

## Cockpit Plugins hub

Cockpit's plugin UI now lives under the dedicated **Plugins** screen
(`apps/cockpit/src/views/PluginsView.tsx`) plus the per-plugin detail screen
(`PluginDetailView.tsx`), not the old Apps-only catalog flow. Plugin
management — install, update, pin, uninstall, doctor — is Cockpit- and
daemon-only; there is no CLI surface for any of it. The screen is split into
two tabs (`Segmented`), backed by thin Tauri commands:

- **Installed** — DB-backed MCP apps already added to the local machine
  (`apps_cmd.rs` / `useApps`), plus every installed plugin the ledger and
  registries know about (providers, gateways, catalog connectors, and skill
  packs) via `usePlugins`/the `list_plugins` RPC. Every installed card exposes
  Uninstall; skill-pack cards additionally show Update and Pin/Unpin, and a
  "Pinned" pill plus an "Attach failed" pill (from the doctor findings)
  surface inline. A separate "Skill sources" card lists any git-installed
  skill that isn't tied to a plugin id. Above the tabs, an **Update all** button batches
  the `update_all_plugins` RPC across every installed pack (skipping pinned
  ones) and a **doctor** chip shows the current `plugin_doctor` issue count
  (opens `DoctorPanel`, a read-only findings list grouped by severity).
- **Browse** — a pure grid of the embedded **catalog** manifests with a
  category filter, plus any curated skill pack (e.g. Superpowers) offered as
  an installable card. The old MCP-registry browser (`registry_cmd.rs` /
  `registry_search`) has been removed entirely; MCP-server apps installed
  through it keep working as ordinary Apps rows. Hand-adding an MCP server
  stays available via **Add MCP server** (AddAppModal), and hand-adding a
  skill source via **Add skill source** (`SkillInstallModal`).

"Browse" lists the embedded catalog (24 manifests baked into the binary)
merged with any cached [remote catalog](#remote-catalog) entries — a signed
`catalog.json` feed the daemon fetches, verifies, and version-gates over the
embedded set — plus the curated skill packs baked into `skills_install.rs`.
The remote feed can add new ids and override an embedded id's manifest at a
strictly higher semver; it can never delete an embedded entry. A header
"Refresh catalog" button and status line drive this on demand; see the
linked section for the fetch cadence, signing, and publish flow.

Signed WASM component bundles (gateways/connectors/providers — see [WASM
component bundles](#wasm-component-bundles) below) are a separate, additive
mechanism and do not appear in Browse at all. Today only `mimo`/`opencode`
surface anywhere in Cockpit's UI for this mechanism, under the Installed
tab's own "Component plugins" section (install/rollback buttons); `github`,
`atlassian`, `bitbucket`, and `discord` component bundles have no Cockpit UI
yet.

Installing a skill pack (Browse card or "Add skill source") goes through the
two-phase tiered trust gate described in
[Installing skill packs](#installing-skill-packs) below: a curated source
installs immediately, while an arbitrary source stops at a trust-confirmation
step in `SkillInstallModal` before anything touches the live install
directory. Any successful install, update, uninstall, or confirm sets an
in-memory "restart required" flag Cockpit polls via
the `plugins_restart_required` RPC (and the `restartRequired` derived from
`list_plugins`) — a banner ("Restart Cockpit to apply plugin changes.")
appears above the main view (`App.tsx`) until the app is restarted, since
registries are only built once at process startup.

The plugin detail screen (`PluginDetailView.tsx`) additionally renders a
**Provenance** card (source spec, resolved commit prefix, install/update
timestamps — from the `plugin_installs` ledger row) and, when the doctor
has an `attach-failed` finding for this plugin, an **Attach failed** banner
with the recorded reason and a "Configure" shortcut that scrolls to the
Authentication/Settings section.

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
| `description` | string | `""` | Shown via the `plugin_detail`/`list_plugins` RPCs and the Cockpit catalog card. |
| `homepage` | string \| null | `None` | |
| `icon` | string \| null | `None` | A lucide icon name. Cockpit maps a small explicit set (`message-circle`, `terminal`, `cpu`, `globe`, `database`, `search`, `cloud`, `server`, `webhook`, `key`, `mail`, `bot`) and falls back to a generic puzzle icon for everything else — including brand-name icons like `github`, `slack`, or `figma`, since `lucide-react` dropped brand/logo icons (see `apps/cockpit/src/lib/plugin-icons.ts`). |
| `categories` | string[] | `[]` | See the vocabulary table below. Unknown labels are a non-fatal warning (`PluginManifest::warnings()`), never a validation error. |
| `slot` | string \| null | `None` | Exclusive capability claim (first-registration-wins) — see [Exclusive capability slots](#exclusive-capability-slots). |
| `verified` | bool | `false` | First-party/vendor-confirmed. Drives the `verified`/`experimental`/`community` status label (see below). |
| `experimental` | bool | `false` | Docs-only entry with no working `[[mcp]]` server — see [Enabling](#enabling-plugins). |
| `auth` | `[auth]` table \| null | `None` | See [Auth kinds](#auth-kinds-and-substitution). |
| `settings` | `[[settings]]` array | `[]` | Extra non-auth settings fields. |
| `mcp` | `[[mcp]]` array | `[]` | See [MCP server defs](#mcp-server-defs). |
| `extensions` | `[[extension]]` array | `[]` | Supervised subprocess "code plugin" declarations (Track D) — see the `[[extension]]` reference below. |
| `skills` | `[[skills]]` array | `[]` | See [Skills bundling](#skills-bundling). |
| `provider` | `[provider]` table \| null | `None` | Model-provider capability block — see below. |

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
| `options` | string[] | `[]` | Non-empty makes this field an enum/choice: the persisted value must be one of these members, enforced at write time by `settings::store::validate_plugin_field`. `validate()` rejects a non-empty `options` paired with `kind != "string"` (`SettingOptionsRequireStringKind`). |
| `default` | string \| null | `None` | Effective value used when no row is persisted yet. When `options` is non-empty, `default` (if set) must be one of its members — `validate()` rejects otherwise (`SettingDefaultNotInOptions`). |

Example — an enum field with a default:

```toml
[[settings]]
key = "plugin.acme.tier"
label = "Tier"
kind = "string"
options = ["free", "pro", "enterprise"]
default = "free"
```

Cockpit's plugin detail screen (`PluginDetailView.tsx`'s `FieldRow`) renders
a field by `kind`/`options`: `kind = "bool"` is a self-saving `Switch`
(using `default == "true"` for its initial toggle state when no row is
set); a non-empty `options` list is a `Combobox` of those choices,
regardless of `secret`; otherwise it's a plain `Input`, typed `number` when
`kind = "int"` and password-masked when `secret = true` — `int` wins that
tie-break, so a secret `int` field still renders as a plain number input,
not masked. Outside the `bool`/`Combobox` cases, `default` is shown only as
placeholder text (`"Default: <value>"`), never pre-filled into the input.
On the read path (used for `${setting:KEY}` substitution),
`SettingsStore::get` (`crates/core/src/settings/store.rs`) resolves a value
in this order: the persisted row, then the static settings catalog's own
default, then this field's manifest `default`, then `None`.

### `[[mcp]]`

See [MCP server defs](#mcp-server-defs) for the full field table.

### `[[extension]]`

`ExtensionDef` (`ryuzi_plugin_sdk::manifest::ExtensionDef`) — a supervised
subprocess "code plugin" declaration (Track D). See
[Extension runtime](#extension-runtime-track-d) below for what actually
runs one of these.

| Field | Type | Default | Notes |
| --- | --- | --- | --- |
| `name` | string | *(required)* | Unique within this manifest's own `extensions` list — NOT globally, and a separate namespace from `[[mcp]]` server names (an extension and an MCP server may share a name). |
| `command` | string | *(required)*, non-empty | The stdio binary to spawn, or a `${...}` placeholder (`${auth}`, `${setting:KEY}`) resolved the same way `McpServerDef::command` is. |
| `args` | string[] | `[]` | |
| `events` | string[] | `[]` | Hook events this extension subscribes to. Every entry must be a member of `KNOWN_HOOK_EVENTS` (`session.start`, `tool.before`, `tool.after`, `session.end`) — an unknown event is a hard `validate()` error (`ExtensionUnknownEvent`), unlike an unknown `categories`/`slot` value, which only warns. |
| `provides_tools` | bool | `false` | If true, the host queries this extension for tool definitions at init and wires them into the session's tool registry. |
| `timeout_ms` | integer \| null | `None` | Per-event response budget in milliseconds. When present, must be `> 0` and `<= 60000` (`MAX_EXTENSION_TIMEOUT_MS`), or `validate()` rejects it (`ExtensionTimeoutOutOfRange`). The runtime falls back to 5000ms (`DEFAULT_EVENT_TIMEOUT_MS`) when omitted. |

`validate()` also rejects a duplicate `name` within one manifest
(`DuplicateExtensionName`), an empty `command` (`ExtensionEmptyCommand`),
and a `${auth}` placeholder in `command`/`args` with no `[auth]` block
(`ExtensionAuthPlaceholderWithoutAuth`).

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

`[provider]` is populated by the generated built-in provider plugins
(`providers.rs`); none of the 24 embedded catalog manifests use it — a
third-party integration is a connector, not a model provider.

Separately from this declarative `[provider]` field, a model-provider
capability can ALSO ship as a signed WASM component bundle — see [WASM
component bundles](#wasm-component-bundles) below. That path is
**transitional**: native `llm_router` routing remains the primary, default
path for every provider; a routed connection only diverts to an in-process
component when one is installed and enabled for that provider id (today
that happens automatically only for the `mimo`/`opencode` free tiers — see
below), and no provider component has been validated against a live vendor
endpoint yet.

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

Validating it: there is no CLI surface for this (the runner's command
surface is `setup`, `start`, `status`, `service`, `config`, `doctor` — no
`plugins` subcommand). The daemon's `plugin_detail` RPC (`{ id: "github" }`)
returns the same fields as JSON — `id`, `name`, `version`, `publisher`,
`description`, `categories`, `status` (`verified`/`experimental`/
`community`), `capabilities`, `enabled`, `auth`, and `mcp` — and Cockpit's
plugin detail screen renders them directly.

(`status` is `verified` when `verified = true`; otherwise `experimental` when
`experimental = true`; otherwise `community`.)

---

## Category vocabulary

`ryuzi_plugin_sdk::categories::KNOWN` — 22 standard labels. Unrecognized
categories are a warning, not a validation error, so the vocabulary can grow
without breaking existing manifests.

| Category | Used for |
| --- | --- |
| `model-provider` | Every LLM API provider (paired with `api-key`/`oauth`/`free` below) |
| `api-key` | A model provider authenticated by API key (e.g. Anthropic, OpenAI) |
| `oauth` | A model provider authenticated via OAuth (e.g. `anthropic-oauth`, `openai-oauth`) |
| `free` | A free-tier model provider (e.g. `kiro`, `opencode-free`) |
| `runtime` | The in-process native agent runtime (`native`) |
| `chat-gateway` | A chat platform gateway (the `discord` WASM component bundle) |
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
| `skills` | Reserved for skill-pack plugins; no shipped catalog or generated skill-pack manifest sets it yet (`generated_plugin_manifest` always emits `categories = []`) |

`model-provider`/`api-key`/`oauth`/`free` are provider-only labels generated
by `providers.rs`, not something a third-party integration manifest needs —
an integration's auth tier is described by `[auth].kind`, not by category.

---

## Exclusive capability slots

A category (above) is a free-form, cosmetic tag any number of plugins may
share. A `slot` is stricter: it's a plugin's claim to be *the* provider of
one named capability — e.g. a Hermes memory backend declaring:

```toml
slot = "memory"
```

`ryuzi_plugin_sdk::categories::KNOWN_SLOTS` names three recognized slots —
`memory`, `knowledge-graph`, `search` — but, like `categories`, an
unrecognized slot name is only a non-fatal `PluginManifest::warnings()`
entry, never a `validate()` error.

Arbitration happens in `PluginHost::add` (`crates/core/src/plugins/host.rs`)
at plugin-registration time, using the same first-registration-wins rule the
host already uses for duplicate plugin `id`s: the first plugin registered
that claims a given `slot` becomes its owner (`PluginHost::slot_owner(slot)`);
every later plugin claiming the same slot is still registered as an ordinary
plugin (its other capabilities work normally) but loses the slot claim, and
is recorded in `PluginHost::slot_conflicts()`. `plugin_doctor`
(`crates/core/src/plugins/doctor.rs`) surfaces every recorded conflict as a
`"slot-conflict"` warning finding, attributed to the losing plugin, naming
both the winner and the loser.

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
parse error. Nothing surfaces to the Cockpit UI mid-session beyond that
log line, so check the `plugin_detail` RPC's (or the Cockpit plugin
detail screen's) `auth`/`setting` fields *before* enabling a plugin,
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

## Approval attribution

When a plugin-provided tool (an MCP server's tool, or — Track D — an
extension's `provides_tools` tool) needs approval, the approval prompt shows
which plugin owns it. `crate::domain::Principal { plugin_id, plugin_name }`
is "attribution only" (its own doc comment: it "carries no gating
semantics") — resolved once, at the point a tool is bound to its owning
plugin, never parsed back out of a tool name:

- For an MCP server's tools: `ControlPlane::attach_plugin_mcp_servers`
  (`crates/core/src/control/lifecycle.rs`) builds a `name -> Principal` map
  from the manifest that contributed each `McpServerSpec`.
- For an extension's tools (Track D): `ExtensionHost::spawn_all`
  (`crates/core/src/plugins/extension/proc.rs`) resolves the owning plugin's
  `Principal` once per plugin at spawn time; every `ext__<extension>__<tool>`
  tool it provides carries that same `Principal` unconditionally — unlike an
  MCP tool, which may have none if it was DB-configured with no owning
  plugin.

The `Principal` rides along on `domain::ApprovalRequest.principal` and the
`approvalRequested` event Cockpit receives; Cockpit's `ApprovalCard`
(`apps/cockpit/src/components/approval/ApprovalCard.tsx`) is what actually
renders it, as a `via {pluginName}` pill next to the request.

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
installer (Cockpit's Plugins hub — the Browse tab's per-card Install button
for curated packs, or the header's **Add skill source** button for an
arbitrary `owner/repo` — backed by the same core path in
`crates/core/src/skills_install.rs`). `install_plugin_pack` writes:

- the pack repo to `~/.config/ryuzi/plugins/<plugin-id>/` (including its
  `ryuzi-plugin.toml`),
- a `.ryuzi-skill.json` provenance stamp **into that plugin directory**,
- and each bundled skill, materialized to
  `~/.config/ryuzi/skills/<plugin-id>--<skill-id>/` with its own
  provenance file.

`load_skill_pack_plugins` (called at startup by `build_daemon`, used by both
the runner's `ryuzi start` and Cockpit's `--engine-daemon` mode) scans
`~/.config/ryuzi/plugins/*/ryuzi-plugin.toml` and registers a directory only
when:

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

The `.ryuzi-skill.json` stamp above is still exactly what the loader checks
at startup — that has not changed. What *is* new (this section) is a
separate SQLite ledger that makes an installed pack's origin, freshness, and
trust state queryable and manageable without re-deriving them from the
filesystem, plus the machinery (atomic staged installs, update/pin, a
two-phase trust gate, attach-status + doctor) built on top of it.

### The install ledger

Every skill/plugin-pack install or update writes one row to a `plugin_installs`
table in the same SQLite database as the rest of Ryuzi's state (see the
`plugin_installs` schema in `crates/core/src/store.rs`, and
`PluginInstallRecord`):

| Column | Type | Notes |
| --- | --- | --- |
| `plugin_id` | TEXT, primary key | |
| `kind` | TEXT | `"plugin_pack"` or `"single_skill"` |
| `source_spec` | TEXT | The install source as given (`owner/repo`, a full GitHub URL, or a curated alias) |
| `resolved_commit` | TEXT, nullable | The git HEAD SHA captured at clone time; `NULL` for rows created by the one-time backfill (the original clone is long gone) |
| `fingerprint` | TEXT | A content hash of the installed tree (excludes `.git` and the `.ryuzi-skill.json` stamp itself) — the local-edit guard's baseline |
| `installed_at` / `updated_at` | INTEGER | Unix ms; `installed_at` is preserved across updates, `updated_at` bumps on every successful reinstall |
| `pinned` | INTEGER (bool) | See [pin](#update-pin-and-the-local-edit-guard) below |
| `pin_reason` | TEXT, nullable | Free-form, set by the pin caller |
| `trust_tier` | TEXT | `"curated"` or `"acknowledged"` today; `"blocked"` is reserved for a future remote-catalog verdict feed (not implemented — see [Two-phase tiered trust gate](#two-phase-tiered-trust-gate)) |
| `trust_ack_at` / `trust_ack_summary` | INTEGER / TEXT, nullable | When and what the user acknowledged (a JSON snapshot of the `TrustPrompt` they saw); `NULL` for curated installs and backfilled rows |

The ledger is **the record of record** for install management (what to show
in the UI, what `update`/`pin`/`uninstall` operate on) — the on-disk
`.ryuzi-skill.json` stamp remains the loader's own provenance gate and is
untouched by any of this. The two can, in principle, drift (e.g. someone
hand-deletes the ledger's SQLite file); nothing here reconciles that beyond
the one-time backfill described next.

`backfill_install_records` runs once per `ControlPlane` construction (daemon
and Cockpit both go through it) and creates a ledger row for every on-disk
pack that doesn't have one yet — installs made before this ledger existed.
It's idempotent (skips any `plugin_id` already recorded) and best-effort: a
backfill failure is logged and never blocks startup. Backfilled rows have no
`resolved_commit` and get a fresh `installed_at`/`updated_at` timestamp, and
their `trust_tier` is inferred the same way a fresh install's is — `"curated"`
for a `CURATED_SKILL_SOURCES` repo, `"acknowledged"` otherwise.

### Atomic staged install

Every install/update/confirm — single skill or plugin pack — writes into a
staging directory first and only swaps it into the live location once
everything for that call has succeeded. For a plugin pack (`install_plugin_pack`
in `skills_install.rs`) this is a multi-directory atomic swap (`DirSwap`): the
plugin directory and every one of its bundled skills' materialized copies are
each staged as siblings of their final target, then `DirSwap::commit` moves
every pre-existing target aside to a backup, renames each staging directory
into place, and — if *any* rename in the batch fails — restores every backup
it had already displaced and removes the half-written staging directories.
Stale-artifact removal (deleting a skill that a pack update no longer
bundles) only runs after a successful commit, so a failed install or update
never deletes still-valid pre-existing files. Net effect: an install that's
interrupted midway (process killed, disk full, a bad git clone) never leaves
an existing pack partially overwritten or missing.

### Update, pin, and the local-edit guard

`update_installed_pack(id, force)` re-resolves the pack's *recorded*
`source_spec` (not whatever is currently on disk) into a fresh temp clone and
walks a fixed decision order — see `UpdateOutcome`:

1. No ledger record for `id` → `Failed("no install record for {id}")`.
2. `pinned` → `SkippedPinned`. Pinning is an unconditional choice; `force`
   does not override it.
3. The on-disk fingerprint no longer matches the recorded one (the user
   hand-edited the installed files) → `LocalEdits`, unless `force` is set.
4. The freshly re-cloned commit equals the recorded `resolved_commit` →
   `AlreadyCurrent`, unless `force` is set.
5. The re-clone bundles a hook script the recorded `trust_ack_summary`
   doesn't already cover → `NeedsReack(TrustPrompt)`. This check runs
   **regardless of `force`** — hook scripts execute code on every matching
   tool call, so `force` is not allowed to skip re-acknowledging them. The
   caller resolves this the same way as a fresh install: pass the prompt's
   `token` to `confirm_install`.
6. Otherwise: reinstall via the same staged `DirSwap` path as a fresh
   install, clean up any artifacts the update no longer produces, and
   rewrite the ledger row with the new `resolved_commit`/`fingerprint`/
   `updated_at` (preserving `installed_at`, `pinned`, `pin_reason`, and the
   existing trust fields from the old row) → `Updated`.

`update_all_packs` runs this for every ledger row, skipping pinned ones, and
never fails as a whole — one pack's error becomes an `UpdateOutcome::Failed`
entry in its result list so the rest of the batch still completes.

`set_pack_pin(id, pinned, reason)` is a thin passthrough that flips
`plugin_installs.pinned`/`pin_reason` — the ledger row is the single source
of truth `update_installed_pack` checks; pinning doesn't touch anything on
disk or require a restart.

### Two-phase tiered trust gate

Installing (or updating into) an **arbitrary** source never touches the live
install directory in one step. `begin_install(source)` classifies the source
first:

- **Curated** (today: Superpowers, `obra/superpowers` / its GitHub URL, via
  `CURATED_SKILL_SOURCES`) — installs immediately and records
  `trust_tier = "curated"`. An explicit Cockpit-driven install of a
  curated pack *is* the trust decision; there's no extra prompt.
- **Everything else** — clones into a temp directory, discovers what it would
  install (skills, bundled `.ryuzi/hooks/<event>/<script>` files, total byte
  size), and returns `BeginInstall::NeedsConfirmation(TrustPrompt)` instead of
  writing anything live. The clone is held in a process-global staging map
  keyed by a random `token`, good for **10 minutes** — an expired or replayed
  token makes `confirm_install` fail with "install session expired".

`TrustPrompt` — the payload shown to the user before they can proceed —
carries: `token`, `sourceSpec`, `ownerRepo`, `resolvedCommit`, the discovered
`skills` list, the `hookScripts` list (`<event>/<script>`, sorted), and
`totalBytes`. Cockpit's `SkillInstallModal` renders exactly this: source,
repo, commit, size, the skill list, and — called out with a warning style —
any bundled hook scripts, since those "run automatically when triggered."
The user must click "Trust & Install" (which calls `confirm_install(token)`)
before anything is written; there's no way to skip the prompt for a
non-curated source.

`confirm_install` is single-use (the token is removed from the staging map up
front) and completes the install from the *staged* clone — via the same
atomic `DirSwap` path — recording `trust_tier = "acknowledged"`,
`trust_ack_at = now`, and `trust_ack_summary` = a JSON snapshot of what was
shown (source, repo, commit, skills, hook scripts). That snapshot is what
later update calls diff against for re-ack-on-hook (step 5 above): a hook
script already listed in `trust_ack_summary.hookScripts` doesn't re-trigger
the prompt, but a genuinely new one does — including for a curated or
backfilled pack, whose `trust_ack_summary` is `None` (nothing acknowledged),
so *any* hook script found on its next update trips the gate once.

A third tier, `"blocked"`, is reserved in the `trust_tier` column for a
future remote-catalog verdict feed — nothing in this milestone sets it, and
there is no remote/community catalog shipped to source a verdict from.

### Attach status and doctor

Session start (`control::lifecycle::attach_plugin_mcp_servers`) records one
`plugin_attach_status` row per connector-capable, enabled plugin it attempts
to attach — `plugin_id`, `last_attach_at`, `outcome` (`"ok"` /
`"failed"`), and a secret-free `reason` (the same friendly text `ensure_auth`
already produces, e.g. `"configure {id}: see {help_url}"` — never a raw
credential or unresolved-placeholder parse error). This is the same table
`doctor.rs` reads for its `attach-failed` finding and what Cockpit's plugin
detail screen shows as the "Attach failed" banner.

`crate::plugins::doctor::plugin_doctor` (`crates/core/src/plugins/doctor.rs`)
is a **read-only** aggregation — it never mutates settings, the store, or
plugin state — over every plugin the host knows about, producing a flat
`Vec<DoctorFinding>` (`plugin_id`, `severity` = `"warn"`/`"error"`, `kind`,
`message`, `suggested_action`):

| `kind` | Severity | Trigger |
| --- | --- | --- |
| `missing-binary` | error | An enabled connector's `[[mcp]]` stdio `command` isn't on `PATH` |
| `reconnect-required` | warn | A stored OAuth token is flagged `reconnect_required` |
| `attach-failed` | warn | The plugin's last recorded `plugin_attach_status` row has `outcome = "failed"` |

Every `message`/`suggested_action` is verified secret-free by tests (no
`refresh_token`/`client_secret` substrings can appear). Cockpit surfaces this
via a "Doctor: OK" / "N issues" chip on the Plugins hub (opens `DoctorPanel`,
findings grouped errors-first) and, per-plugin, as the attach-failure banner
on the detail screen.

### Daemon RPC methods (`POST /rpc/{method}`)

The engine daemon (`ryuzi start` / `--engine-daemon`) exposes plugin
management as RPC methods, dispatched by `crate::api::plugins_api::dispatch`
(routed from `crate::api::dispatch` in `crates/core/src/api/mod.rs`). Cockpit
calls each through a thin `EngineClient::rpc` proxy in
`apps/cockpit/src-tauri/src/plugins_cmd.rs` — there are no REST plugin routes
on the HTTP router anymore; `serve.rs` only owns `/rpc/{method}`, `/events`,
and `/approvals`. Every method's params object uses the Rust snake_case
parameter names; the Tauri command name matches the RPC method name 1:1.

| Method | Params | Result | Notes |
| --- | --- | --- | --- |
| `list_plugins` | — | `PluginInfo[]` | Every plugin as a compact summary, enriched with its ledger fields (see below) |
| `plugin_detail` | `{ id }` | `PluginDetail` | The plugin's full manifest plus `enabled`/`source` and the same ledger enrichment |
| `plugin_doctor` | — | `DoctorFinding[]` | `plugin_doctor`'s findings array |
| `begin_skill_install` | `{ source }` | `SkillInstallBegin` | Phase 1 of the trust gate — curated sources install immediately (sets restart-required), arbitrary sources return `{ completed: false, trust: TrustPrompt }` |
| `confirm_skill_install` | `{ token }` | `InstalledSkillPack` | Phase 2 — completes a staged install/update after acknowledgment; sets restart-required |
| `update_plugin` | `{ id, force }` | `UpdateOutcomeDto` | Runs `update_installed_pack`; only an `Updated` outcome sets restart-required |
| `update_all_plugins` | — | `UpdateOutcomeEntry[]` | Runs `update_all_packs`; sets restart-required if at least one pack actually reinstalled |
| `set_plugin_pin` | `{ id, pinned, reason? }` | — | Flips the ledger's pin state; never sets restart-required (pin doesn't change what's on disk or loaded) |
| `uninstall_plugin` | `{ id }` | `PluginInfo[]` | Uninstalls a recorded pack via the recorded remove path: removes it from disk and deletes its `plugin_installs`/`plugin_attach_status` rows; sets restart-required; returns the refreshed list |
| `plugins_restart_required` | — | `boolean` | Reads the in-memory restart-required latch (see below) |

`list_plugins` and `plugin_detail` merge in, when a `plugin_installs` row
exists for that id: `sourceSpec`, `resolvedCommit`, `pinned`, `installedAt`,
`updatedAt`, and `trustTier` (see `install_ledger_index` /
`InstallLedgerFields` in `plugins_api.rs`) — note `sourceSpec` is deliberately
a distinct key from the existing `source` field, which stays the stable
`"builtin" | "catalog" | "skill-pack"` enum label. `list_plugins` fetches the
ledger once (`store.list_plugin_installs`) and indexes it by id so list
assembly never does a per-plugin store round-trip; `plugin_detail` uses the
single-id `store.get_plugin_install`.

`restartRequired` is a single in-memory flag on the daemon's `ControlPlane`
(`mark_plugins_restart_required`/`plugins_restart_required`), not persisted —
it's set by any install/update/uninstall that actually changed what's on
disk or should be loaded, and read back via the `plugins_restart_required`
RPC. It exists because plugin registries (`Registries`) are built exactly
once at process startup (see [the wiring note above](#two-layers-manifest-vs-coreplugin));
there is no hot-reload, so a fresh install genuinely isn't live until the
daemon restarts. Nothing today clears the flag except a daemon restart —
Cockpit is a thin client, so the flag lives on the daemon (the actual plugin
host), and a daemon restart naturally starts a new process with it reset to
`false`.

---

## Enabling plugins

`PluginHost::is_enabled` (`crates/core/src/plugins/host.rs`) resolves
enablement by capability, in this priority order:

1. Unknown `id` → `false`.
2. `native` (the built-in agent harness) → always `true`; the agent is not
   toggleable.
3. Gateway-capable (a native `CorePlugin` with a gateway factory) → whether
   `id` is in the `enabled_gateways` CSV setting. There is no built-in native
   gateway today; WASM gateway component bundles are enabled via
   `plugin.<id>.enabled` instead (see `component_plugin_enabled`).
4. `experimental = true` (docs-only: `ngrok`, `vercel-sandbox`, `zep`) →
   always `false` — there is no capability to enable, and this wins even if
   a stray `plugin.<id>.enabled = true` row exists.
5. No harness/gateway/connector capability at all (every model-provider
   plugin) → always `true`.
6. Otherwise (every catalog/skill-pack integration with a connector) →
   `plugin.<id>.enabled == "true"`, defaulting to `false`.

Two equivalent ways to flip it — there is no CLI surface for either:

- **Cockpit**: the dedicated **Plugins** screen's **Browse** tab lists every
  catalog plugin with a category filter; new installs go through the
  Install wizard, and each installed card keeps the enable/disable
  `Switch` (disabled — greyed out — for `experimental` entries, since
  there's nothing to enable); the plugin detail screen (reached from the
  sidebar's Plugins section or the Browse card's "Configure" button) has
  the same switch. Cockpit's `set_plugin_enabled` Tauri command
  (`{ id, enabled }`) is what the switch calls.
- **Settings keys directly**: `enabled_gateways` is a CSV
  string (add/remove `id`, preserving order, no duplicates —
  `toggle_enabled`'s `toggle_csv` helper); everything else is
  `plugin.<id>.enabled` (`"true"`/`"false"`). Cockpit's `set_plugin_enabled`
  command delegates to `ryuzi_core::plugins::toggle_enabled` — the single
  source of truth both the settings rows and [`PluginHost::is_enabled`]'s
  read side agree with.

For example, `set_plugin_enabled({ id: "github", enabled: true })` flips
`plugin.github.enabled` to `"true"`; the next `list_plugins` RPC call (or
Cockpit's plugin card) reflects `enabled: true` immediately.

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

Every id's `CorePlugin` comes from one of the three `PluginHost` sources
above. The last two rows below are a DIFFERENT thing — signed WASM component
bundles are never `PluginHost` entries (see [WASM component
bundles](#wasm-component-bundles)) — but are listed here too since they
answer the same "where does id X come from" question.

| Plugin id(s) | Source | Capability |
| --- | --- | --- |
| `native` | `harness::native::native_plugin()` | harness (in-process agent loop) |
| `discord` | signed WASM component bundle (`plugins/discord`), discovered off-disk | gateway (via `wasm_gateway_bridge::WasmGateway`) |
| `anthropic`, `openai`, `ollama`, ... (every `llm_router::registry::CATALOG` entry) | `plugins::providers::provider_plugins()` | manifest-only, `[provider]` block; category `model-provider` + `api-key`/`oauth`/`free` |
| `github`, `atlassian`, ... (24 entries) | Embedded TOML, `plugins::catalog::catalog_plugins()` | connector (via `declarative_plugin`) — `github`/`atlassian` ALSO have a separate WASM connector bundle, see below |
| `github`, `atlassian`, `bitbucket` | signed WASM component bundle (`plugins/<id>`), discovered off-disk — NOT a `PluginHost` entry | connector (via `wasm_connector::WasmToolSet`, in-process session tools, not `[[mcp]]`) |
| `mimo`, `opencode` (auto-installed); `openai`, `anthropic`, `qwen`, ... (Task-16c ports, installable but not auto-installed) | signed WASM component bundle (`plugins/<id>`), discovered off-disk — NOT a `PluginHost` entry | provider transport (via `wasm_provider`), diverts `llm_router` routing for its declared `provider-ids` when installed + enabled |

---

## Hook scripts

Plugins that need to observe or gate tool calls (rather than contribute an
MCP server) use hook scripts, not the manifest — see
`crates/core/src/harness/native/hooks.rs`. Scripts live at:

```
.ryuzi/hooks/<event>/
```

one executable per file, run in filename-sorted order, receiving the event
payload as JSON on stdin. The typed event vocabulary (`hooks::HookEvent`) has
four members, all dispatched from the native runtime (`harness::native`):

| Event | Fires | Gating? | Payload |
| --- | --- | --- | --- |
| `session.start` | `NativeHarness::start_session`, after model/agent resolution | No (observational) | `{ session, project, model, work_dir }` |
| `tool.before` | `harness::native::runner`, before every tool call | **Yes** | `{ tool, input }` |
| `tool.after` | `harness::native::runner`, after the tool call resolves (Ok or Err) | No (observational) | `{ tool, input, result: { ok, output } }` or `{ tool, input, result: { ok: false, error } }` |
| `session.end` | `NativeSession::end` — the sole teardown path reached from `ControlPlane::end_session` (never from a `stop_session` interrupt) | No (observational) | `{ session, reason }` |

Only `tool.before` is gating: a non-zero exit denies the action and the
script's stdout becomes the shown reason. Every other event is
fire-and-forget — a non-zero exit from an observational hook is ignored
(the remaining scripts still run, and the tool/session outcome is
unaffected). `tool.after`'s `result.output`/`result.error` is truncated
(2,000 bytes) before being written to the hook's stdin; it is not a
secrets-scrubbed channel beyond that — it carries the same model-facing text
the tool already returned to the LLM, not raw untouched process output. A
missing hooks directory, or a script that isn't executable, is treated as
"allow" — never a hard failure.

On-disk scripts are one of TWO sinks for the same typed `HookEvent`
vocabulary. `harness::native::hooks::fire_hook` — the one call site every
`harness::native` hook fire site actually uses — runs the on-disk scripts
(`run`, above) and, when the session has any spawned extension subscribed to
the event, Track D's supervised extension subprocesses
(`plugins::extension::events::ExtensionEvents::dispatch`) CONCURRENTLY, then
combines their results: for `tool.before`, either sink denying denies the
call (a script-deny always wins even if every extension allowed, and vice
versa); every other event stays fire-and-forget from both sinks. A session
with no extensions registered (`SessionCtx.extension_events: None`, the
common case) skips the extension side entirely and behaves exactly as if
Track D didn't exist. See [Extension runtime](#extension-runtime-track-d)
below for what a subscribed extension actually receives and how it answers.

---

## Extension runtime (Track D)

An **extension** (the `[[extension]]` manifest table above) is a supervised
**subprocess**, never in-process plugin code: `plugins::extension`'s own
module doc states this as a hard invariant — "every extension is a
subprocess... no mechanism to load or execute plugin-supplied code any
other way." It speaks JSON-RPC 2.0 over its own stdin/stdout, subscribes to
the same `HookEvent` vocabulary [hook scripts](#hook-scripts) use, and —
optionally — exposes tools into a session's tool registry.

### Extension vs. `[[mcp]]`

Both are subprocess integrations, but they cover different axes:

| | `[[mcp]]` | `[[extension]]` |
| --- | --- | --- |
| Provides tools | Yes — that's its only job | Optional (`provides_tools`) |
| Reacts to lifecycle events | No | Yes (`events[]`) |
| Can gate/deny a tool call | No | Yes, if subscribed to `tool.before` |
| Spawned | Per session, at attach | Once per daemon lifetime — every subscriber shares one long-lived process |

### Protocol (`plugins::extension::protocol`)

Every method is plain JSON-RPC 2.0 over stdio, framed one JSON object per
line. The host speaks `PROTOCOL_VERSION = "1"`.

| Method | Direction | Purpose |
| --- | --- | --- |
| `extension/initialize` | host -> extension | One-time startup handshake — see below. |
| `event/<name>` | host -> extension | Fires a subscribed `HookEvent` (`event/tool.before`, `event/tool.after`, `event/session.start`, `event/session.end`), carrying the same JSON payload the on-disk script sink receives on stdin. |
| `extension/ping` | host -> extension | Health probe (the supervisor below). |
| `tool/call` | host -> extension | Invoke one `{ name, arguments }` tool this extension declared via `provides_tools`. |
| `extension/shutdown` | host -> extension | Request a graceful stop. |

**Handshake.** The host sends `extension/initialize` with
`{ protocolVersion, host: { name, version }, events: [...], providesTools }`
— `events` is every hook event this extension's manifest subscribes to,
`providesTools` mirrors its `provides_tools` flag. A well-behaved extension
replies `{ result: { ok: true, events: [...], tools: [...] } }` — `events`
is which of the offered events it actually confirms (may be a subset),
`tools` is present only when it declared `provides_tools`, each a
`{ name, description?, inputSchema? }` def (`name` is the only required
field; a missing/blank one is skipped, not fatal). `protocolVersion` in the
response is checked only if the extension bothers to send one.

**Event dispatch.** `event/<name>` carries the event's JSON payload verbatim
as `params` — identical to what the on-disk script sink gets on stdin (see
[Hook scripts](#hook-scripts) for the payload shape per event). For a
gating event (`tool.before`), the extension answers
`{ result: { deny: true, reason: "..." } }` to deny, or anything else
(including an empty `{}` or `{"result":{"deny":false}}`) to allow.
Non-gating events are sent the same way but the response is never awaited
on the session's hot path (see Security below).

**Tool calls.** `tool/call` carries `{ name, arguments }`; the extension
replies `{ result: <value> }` (flattened the same way an MCP tool's reply
is) or a JSON-RPC `error` object, surfaced to the model as a normal tool
ERROR — a rejecting/timing-out/crashed extension never propagates a panic
or a hang.

### Security model (`plugins::extension::proc`)

Every extension child is spawned with `Command::env_clear()`, **not** the
daemon's inherited environment — this is deliberately stricter than the
native MCP client (`harness::native::mcp_client`), which still layers onto
the daemon's full inherited environment for `[[mcp]]` stdio servers. An
extension child gets only:

- a minimal safe base copied from the daemon's own environment when
  present: `PATH`, `HOME`, `LANG` (`SAFE_BASE_ENV_VARS`);
- exactly the extra `(key, value)` pairs its resolved `ExtensionSpec.env`
  declares — always empty today, since `ryuzi_plugin_sdk::ExtensionDef` has
  no `env` table of its own yet (only `command`/`args` can reference
  `${auth}`/`${setting:KEY}`).

**Fail-open on gating.** A `tool.before` dispatch to an extension is bounded
by that extension's own `timeout_ms` (falling back to
`DEFAULT_EVENT_TIMEOUT_MS` = 5000ms when the manifest omits it); a timeout,
a crashed process, or a closed transport is treated as "did not deny" plus
a `tracing::warn!` — a broken/slow extension can never brick the agent.
Every subscribed extension for a gating event is dispatched CONCURRENTLY, so
total wait is bounded by the single slowest extension's own timeout, not
their sum; any one denying denies the call.

**Fire-and-forget on observational events.** `session.start`/`tool.after`/
`session.end` dispatches are never awaited on the caller's path at all —
each subscribed extension's send is a detached background task, capped at
32 concurrently in-flight sends across the whole host
(`MAX_INFLIGHT_OBSERVATIONAL_SENDS`); a send that can't get a slot is
dropped (logged), never queued.

**Sanitized deny reasons.** A gating deny reason is shown to the user/agent,
but an extension is less trusted than a hand-written script, so its reason
gets extra screening: capped at 300 characters
(`MAX_DENY_REASON_CHARS`), and wholesale replaced with a generic
`"[reason withheld: it looked like it might contain a credential]"` marker
if it contains a case-insensitive secret-shaped substring (`token`,
`secret`, `password`, `apikey`, `bearer`, `credential`, ...).

### Supervision (`plugins::extension::proc::supervise`)

Each spawned extension is independently supervised — one giving up never
touches any other extension, plugin, or the daemon:

| Knob | Value | Const |
| --- | --- | --- |
| Health ping cadence | 30s | `PING_INTERVAL` |
| Ping round-trip budget | 5s | `PING_TIMEOUT` |
| Restart backoff, first attempt | 1s | `RESTART_BACKOFF_BASE` |
| Restart backoff cap | 60s | `RESTART_BACKOFF_CAP` |
| Max restart attempts before giving up | 5 | `MAX_RESTARTS_IN_WINDOW` |
| ...inside a sliding window of | 5 minutes | `RESTART_WINDOW` |
| Continuously-`Running` duration that resets the restart budget | 60s | `HEALTHY_RESET_AFTER` |
| One-time handshake budget (distinct from the per-event `timeout_ms`) | 25s | `INIT_HANDSHAKE_TIMEOUT` |
| Graceful-shutdown grace period before a hard kill | 5s | `SHUTDOWN_GRACE` |

Backoff is exponential and capped: `min(1s * 2^attempt, 60s)` — 1s, 2s, 4s,
8s, 16s, 32s, then clamped at 60s. Once `MAX_RESTARTS_IN_WINDOW` (5) restart
*attempts* have happened inside the 5-minute `RESTART_WINDOW`, the
supervisor gives up permanently: the extension's status becomes
`Failed("restart-exhausted: 5 restarts within 300s")` and its task exits —
it never respawns again without a daemon restart. An extension that stays
`Running` continuously for `HEALTHY_RESET_AFTER` (60s) gets its restart
history cleared, so an old, long-past burst of restarts never counts
against a later, unrelated crash.

### Trust — installing an extension-declaring plugin

Installing (or updating into) a plugin pack whose manifest declares any
`[[extension]]` entries **always** requires the two-phase trust gate
(`begin_install` -> `confirm_install`, see
[Two-phase tiered trust gate](#two-phase-tiered-trust-gate)) — it is never
curated-immediate, even for a source in `CURATED_SKILL_SOURCES`:
`skills_install.rs`'s `discovery_runs_code` reports `true` for any pack
whose manifest has a non-empty `extensions` list, and `begin_install_with`
only takes the curated-immediate branch when the source is curated **and**
`discovery_runs_code` is `false` — so a code-running pack always falls
through to `stage_for_trust_prompt` and `BeginInstall::NeedsConfirmation`.
Cockpit's `SkillInstallModal` shows a "Runs code" warning badge and
explanatory copy ("This plugin runs code in a supervised subprocess —
review it carefully before trusting it.") for this case, distinct from (and
in addition to) the existing "bundles hook scripts" warning. Updating an
already-installed pack whose freshly re-cloned manifest runs code (whether
an `[[extension]]` was newly added, or was already there) re-triggers the
same confirmation on **every** such update, unconditionally — not just the
first time a code-running version is seen, since the ledger has no reliable
way to diff "the same acknowledged code" from "changed code." The raw,
single-skill `install_skill` path (bypassing the trust-prompt UI) refuses
outright, with an error naming the two-phase flow, for any source that
isn't a curated, non-code pack — a hand-authored extension-bearing pack can
never sneak in through it.

### Observability

`plugin_doctor` (`crates/core/src/plugins/doctor.rs`) adds four
extension-specific finding kinds, gated on the daemon's `ExtensionHost`
actually having spawned something for *any* plugin (so a thin client that
never spawns extensions reports nothing):

| `kind` | Severity | Trigger |
| --- | --- | --- |
| `not-running` | warn | Plugin is enabled and declares an extension, but the host has nothing spawned for it. |
| `crashed` | warn | The extension's live status is `Restarting`. |
| `init-failed` | error | The extension's status is `Failed(reason)` for a reason other than restart exhaustion. |
| `restart-exhausted` | error | The extension's status is `Failed("restart-exhausted: ...")` — the supervisor gave up. |

The `extension_status` RPC (params-free, dispatched by
`api::extension_status_api`) returns a read-only
`{ pluginId, name, status, restartCount, lastError, confirmedEvents,
toolCount }[]` snapshot — `status` is one of `running` / `starting` /
`restarting` / `failed` / `stopped` / the synthetic `not-running`;
`lastError` is populated only when `status = "failed"`, with the same
sanitized reason `plugin_doctor` uses. Cockpit's plugin detail screen
(`PluginDetailView.tsx`) polls this and renders an "Extension" card per
entry: name, a colored status pill, a restart count when non-zero, and the
last error text when present.

### Known limitations

- **stdio transport only** — no HTTP/network transport for an extension;
  every extension is a local subprocess the daemon spawns.
- **No sandbox beyond environment isolation** — `env_clear()` + the
  allowlist stop a secret leak, but there is no filesystem/network/CPU/
  memory sandbox. An extension is trusted the same way an MCP server is:
  the user installed it deliberately, past the trust-gate prompt above.
- **Sequential startup handshakes** — extensions are spawned and
  handshaken one at a time at daemon start, not concurrently.
- **Cockpit's own in-process engine daemon does not run extension
  supervision.** `ExtensionHost::spawn_all` is called once, as a detached
  background task, only by the standalone `ryuzi serve` / `ryuzi __daemon`
  runner (`crates/cli/src/daemon_cmd.rs`) — Cockpit's own spawned
  `--engine-daemon` subprocess never calls it, so extensions only actually
  run under a standalone `ryuzi` daemon (the same characteristic the
  auto-update manager and the remote-catalog manager's background timer
  already have — see [Fetch pipeline](#fetch-pipeline)). If Cockpit attaches
  to an already-running `ryuzi serve` daemon instead of spawning its own,
  that daemon's extensions (and their supervision) are the ones live.
- **Child-exit detection is only as fast as the next health ping** — a
  crashed extension isn't noticed until the next `PING_INTERVAL` (30s)
  elapses and its ping fails (or immediately, if a gating/observational
  dispatch happens to hit the dead transport first).

### Worked example

`ryuzi-plugin.toml`:

```toml
contract = 1
id = "acme-linter"
name = "Acme Linter"
publisher = "acme"
description = "Lints staged files before every bash/edit tool call."
categories = ["observability"]

[[extension]]
name = "linter"
command = "acme-linter-ext"
args = ["--serve"]
events = ["tool.before", "tool.after"]
provides_tools = true
timeout_ms = 5000
```

The `acme-linter-ext --serve` binary must speak JSON-RPC 2.0, one object per
line, on stdin/stdout. A minimal well-behaved reply to
`extension/initialize`:

```json
{"jsonrpc":"2.0","id":1,"result":{
  "ok": true,
  "events": ["tool.before", "tool.after"],
  "tools": [
    {"name": "lint", "description": "Lint a file", "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}}
  ]
}}
```

From then on it must answer `event/tool.before` (denying with
`{"result":{"deny":true,"reason":"..."}}` when it wants to block a call),
`event/tool.after` (result ignored), `extension/ping` (any non-error reply),
and `tool/call` for `"lint"`
(`{"result":{"content":[{"type":"text","text":"0 problems"}]}}`, mirroring
MCP's own tool-result shape) — and, ideally, `extension/shutdown` by
exiting on its own within the shutdown grace period (5s).

---

## WASM component bundles

A third extension mechanism exists alongside the declarative catalog/
skill-pack manifests above and Track D's supervised extension subprocesses:
a **signed WASM component bundle** — a compiled Wasmtime component that runs
in-process, sandboxed by the Component Model's own import/export boundary
rather than a subprocess. First-party bundle sources live under
`plugins/<id>/` in this repo, one component export per role:

| Role | WIT export | Bundle(s) |
| --- | --- | --- |
| Gateway | `ryuzi:gateway/gateway@0.1.0` | `discord` |
| Connector | `ryuzi:connector/connector@0.1.0` | `github`, `atlassian`, `bitbucket` |
| Provider | `ryuzi:provider/provider@0.1.0` | `mimo`, `opencode`, plus the Task-16c LLM-provider ports (`openai`, `openrouter`, `groq`, `deepseek`, `mistral`, `xai`, `nvidia`, `huggingface`, `google`, `anthropic`, `anthropic-oauth`, `qwen`) |

Each bundle's own manifest is `ryuzi-plugin.toml` — the same filename as the
declarative `PluginManifest` above, but a DIFFERENT schema
(`ryuzi_plugin_sdk::bundle::PluginBundleManifest`,
`crates/plugin-sdk/src/bundle.rs`): `id`, `name`, `version` (semver),
`wit-api` (a semver RANGE the component targets), `lifecycle` (`singleton` /
`per-session` / `per-call`), `component` (the compiled `.wasm` filename),
`[permissions].network` (an outbound hostname allowlist), `[[oauth]]`
profiles, and an optional `provider-ids` list — the LLM-router provider
id(s) a provider bundle serves (e.g. the `mimo` bundle declares
`provider-ids = ["mimo-free"]`; omitted, it falls back to `[id]` —
`PluginBundleManifest::resolved_provider_ids`).

**None of this goes through `PluginHost`/`Registries`.** Component bundles
are discovered and wired entirely outside the three `CorePlugin` sources
listed [above](#two-layers-manifest-vs-coreplugin): they never get a
`PluginHost` entry, so they never appear in the `list_plugins`/
`plugin_detail` RPCs Cockpit's Plugins hub reads (per
`apps/cockpit/src/store-plugins.ts`'s own `FIRST_PARTY_BUNDLE_IDS` comment,
which exists precisely because component bundles are otherwise invisible to
those RPCs). Instead, each role is discovered and wired separately, straight
out of `daemon::build_daemon`:

- **Gateways** — `wasm_gateway_bridge::build_wasm_gateways` pushes one live
  `Gateway` per enabled, discovered bundle straight into the daemon's own
  gateway list (see [How built-ins map to plugins](#how-built-ins-map-to-plugins)
  above).
- **Connectors** — discovered per session start
  (`ControlPlane::build_wasm_session_providers`) and surfaced as in-process
  tools alongside Track D's extension tools via
  `plugins::wasm_connector::WasmToolSet`, NOT as `[[mcp]]`-style external
  servers — `wasm_connector`'s own module doc explains why: the Rust
  `Connector` trait only yields pointers to *external* MCP servers and
  structurally cannot represent a tool that runs in-process.
- **Providers** — discovered once at daemon boot
  (`plugins::wasm_provider::discover_provider_components`) and registered,
  by router provider id, into a process-wide transport table;
  `llm_router::client.rs` checks that table BEFORE its generic HTTP `match
  target.desc.format` and diverts to the component when a transport is
  registered for that connection's provider id — the exact "transitional,
  native-primary" seam described [above](#provider-model-provider-plugins).

All three share one enablement key: `plugin.<id>.enabled`
(`plugins::host::component_plugin_enabled`) — the same setting-key
convention a declarative catalog connector uses (see [Enabling
plugins](#enabling-plugins)), but read directly rather than through
`PluginHost::is_enabled`.

### `github`/`atlassian`: two connectors coexist under the same id

`github` and `atlassian` are each BOTH a declarative embedded-catalog
connector (`crates/core/plugins/catalog/{github,atlassian}.toml`, still 2 of
the 24 manifests in `CATALOG_MANIFESTS` — a token/PAT-authenticated HTTP MCP
server pointed at the vendor's own hosted remote MCP endpoint, unchanged
from before) AND a first-party WASM connector component (`plugins/github`,
`plugins/atlassian` — OAuth-authenticated, in-process tools, no MCP server
involved). The two are additive, not a replacement of one by the other, and
— because they share the same id — both are gated by the same
`plugin.<id>.enabled` setting if a deployment ever has both installed.
`bitbucket` has no catalog manifest at all (verified: no `bitbucket.toml`
under `crates/core/plugins/catalog/`, and it never had one) — it exists
ONLY as a WASM connector component.

### Installation, signing, and today's actual reach

A bundle release ships as four artifacts — the manifest, the compiled
`.wasm`, a `PluginRelease` JSON descriptor, and a `plugin.sig`
detached-signature envelope over the release JSON's exact raw bytes —
fetched and verified by `ComponentBundleInstaller`/`install_component_release`
(`crates/core/src/plugins/{bundle,remote_catalog}.rs`) into
`installed_bundle_root()` (`~/.config/ryuzi/plugins` by default). This
mirrors the [remote catalog](#remote-catalog)'s feed-signing design one
layer down — and, like that feed, **its trusted signing key is still the
all-zero placeholder** (`FIRST_PARTY_KEY_ID` in
`crates/core/src/plugins/first_party_key.rs`): `first_party_trusted_keys()`
returns an EMPTY map while the placeholder is in place, so `verify_bundle`
trusts nothing and NO first-party component — `mimo`, `opencode`, or any
connector/gateway — can actually be installed in a real deployment until a
maintainer completes the same one-time key-rollout step already documented
under [Signing](#signing) above. This is a deliberate fail-closed default,
not a bug.

Two further gaps between "the generic machinery exists and is tested" and
"a user can turn this on today":

- **Auto-bootstrap and Cockpit's UI cover only `mimo`/`opencode`.**
  `daemon::build_daemon` auto-installs `FIRST_PARTY_BUNDLE_IDS = ["mimo",
  "opencode"]` on first run (`bootstrap_first_party_components`), and
  Cockpit's Installed tab "Component plugins" section and its install/
  rollback buttons only ever ask about those same two ids
  (`apps/cockpit/src/store-plugins.ts`'s own `FIRST_PARTY_BUNDLE_IDS`
  constant). `github`, `discord`, `atlassian`, and `bitbucket` are built and
  signed by the same publish tooling (`scripts/plugins/build-first-party.ts`
  lists all of them) and are each exercised end-to-end by a dedicated
  integration-test suite that compiles and drives the real component
  (`github_e2e.rs`, `atlassian_bitbucket_e2e.rs`, `discord_e2e.rs`) — but
  none of them is in the auto-bootstrap list or reachable from a Cockpit
  button today; installing one means calling the `install_component_plugin`
  RPC directly.
- **No provider component has been validated against a live vendor
  endpoint.** The e2e suites prove the host-mediated OAuth/HTTP/
  provider-auth plumbing end-to-end against mocks, and the router genuinely
  diverts to a registered provider transport when one exists — but that is
  "the seam is wired," not "shipped and proven against production APIs."
  Treat every provider component as pre-production until stated otherwise.

### Permission model

`[permissions].network` is the only permission axis today (structurally
extensible for a future axis without breaking existing bundles): a bare
lowercase hostname (`api.github.com`) or a `*.`-prefixed wildcard
(`*.github.com`) — no scheme, path, port, IP literal, bare `*`, or uppercase
(`is_valid_network_host` in `crates/plugin-sdk/src/bundle.rs`).
`HostPolicy::for_installed_bundle` (`crates/core/src/plugins/runtime.rs`)
derives every capability grant straight from the manifest plus install
provenance — no grant is ever caller-supplied:

| Grant | Condition |
| --- | --- |
| `allow_network` / `allow_websocket` | Manifest declares at least one `[permissions].network` host |
| `allow_oauth` | Manifest declares at least one `[[oauth]]` profile |
| `allow_provider_auth` | Manifest declares `provider-ids` AND at least one network host (an injected credential needs somewhere to go) |
| `allow_self_auth` | The installed release's recorded `signing_key_id` is the first-party key (gates e.g. `mimo`'s own bootstrap-JWT header) |

The host-injected-credential guarantee: `capabilities::settings` returns an
EMPTY value for a secret field, and `capabilities::http`
(`AllowedHttpClient`) strips any component-supplied `Authorization`/
`x-api-key` header before adding the host's own — a component can request a
credentialed call but can never read, forge, or exfiltrate the credential
itself. Two host-mediated injection capabilities cover this:
`ryuzi:oauth/oauth` (an OAuth *profile*'s bearer token —
`capabilities/oauth.rs`) and `ryuzi:provider-auth/provider-auth` (the user's
stored LLM-provider API key, scoped to the bundle's own declared
`provider-ids` — `capabilities/provider_auth.rs`, Task 16c1). Neither ever
hands the component the raw secret.

### OAuth profiles

A bundle's `[[oauth]]` table (`ryuzi_plugin_sdk::bundle::OAuthProfile`: `id`,
`authorize-url`, `token-url`, `scopes`, `client-id-setting`,
`client-secret-setting`, `resource`, `dynamic-registration`) is a second,
independent OAuth model from the declarative `[auth]` table documented
[above](#auth-kinds-and-substitution) — a different Rust type, different
store tables (`plugin_oauth_profile_tokens`/`plugin_oauth_profile_clients`,
keyed by `(plugin_id, profile_id)`, vs. the single-token-per-plugin
`plugin_oauth_token`), and a profile id distinct from the plugin id (so one
bundle can hold more than one profile). The component never drives the
flow: it only ever calls `authorized-request(profile_id, ...)` /
`disconnect(profile_id)`; PKCE, device flow, refresh, and token storage all
live host-side in `plugins::capabilities::oauth::ProfileOauth`, exposed as
four RPCs (`plugin_profile_begin_pkce`, `plugin_profile_disconnect`,
`plugin_profile_begin_device_flow`, `plugin_profile_poll_device_flow`) —
none of which Cockpit's UI calls yet, so connecting a component's OAuth
profile today is an API-level operation, not a Cockpit button.

`github`'s bundle declares one `github` profile; `bitbucket`'s declares
`bitbucket-cloud`, DELIBERATELY isolated from `atlassian`'s
`atlassian-cloud` profile — Bitbucket Cloud's OAuth consumer is a separate
app registration from Atlassian's Jira/Confluence 3LO app, and the two
grants share no token (`plugins/bitbucket/ryuzi-plugin.toml`'s own comment
on this).

### Revocation & recovery

Two independent revoke surfaces exist, at two different layers:

- The [remote catalog](#the-blocked-denylist)'s `blocked` denylist revokes a
  declarative catalog id.
- The **component-release ledger**'s per-version `revoked` flag
  (`mark_component_release_revoked`, driven by the
  `rollback_component_plugin` RPC) revokes one installed WASM bundle
  release.

`rollback_component_plugin` reactivates the prior-good version FIRST
(`set_active_component_release`, which itself validates the target exists
and is not already revoked) and only THEN revokes the bad one — so a
rollback whose target is missing or already revoked is a clean no-op that
leaves the bad version active, rather than ever stranding the plugin with no
active release (verified by
`rollback_is_a_no_op_when_target_version_is_missing`/
`rollback_is_a_no_op_when_target_version_is_revoked` in
`crates/core/src/api/plugins_api.rs`). A blocked/revoked declarative-catalog
id can never be (re-)enabled going forward — `toggle_enabled`'s `is_blocked`
check refuses the flip outright. For anything already running when a
revoke lands, `apply_blocked_denylist` + `stop_revoked_gateways` gracefully
stop a currently-supervised WASM gateway (calling its own existing
`Gateway::stop`) at a safe boundary, mid-session, rather than waiting for
the next restart; a connector or provider component (never long-lived,
re-resolved every session-attach/daemon-boot) simply picks up the change on
its next attach via the `plugins_restart_required` flag. Every recorded
revoke reason (`revocation_reason`) is operator-authored, secret-free text,
never a raw error or credential.

### Doctor: WASM-component diagnostics

`plugin_doctor` (Task 17b) adds seven new `kind`s on top of the ones
[already documented](#attach-status-and-doctor), all read-only and iterated
generically over the component-release ledger — no plugin-id branch:

| `kind` | Severity | Trigger |
| --- | --- | --- |
| `signature-invalid` | error | The active release's bundle no longer verifies against a trusted key (untrusted `signing_key_id`, or a `verify_bundle` failure) |
| `hash-mismatch` | error | The on-disk `.wasm`'s SHA-256 no longer matches the ledger's recorded checksum (tamper/corruption) |
| `abi-incompatible` | error | The release's `wit-api` range excludes this host's compiled WIT contract version |
| `revoked` | error | A ledger release is recorded `revoked` (independent of the signed-feed `blocked` finding) |
| `policy-violation` | error/warn | The installed manifest fails `PluginBundleManifest::validate()` (error), or declares `provider-ids` without a network allowlist so the host cannot grant `allow_provider_auth` (warn) |
| `oauth-profile-unhealthy` | warn | A declared `[[oauth]]` profile's stored token is missing, reconnect-flagged, or expired |
| `gateway-restart-exhausted` | error | A supervised WASM gateway is down and has already hit its restart backoff ceiling |

Every message/suggested action is generic and secret-free, matching the
existing doctor findings' contract.

---

## Authoring walkthrough

Hand-authored manifests under `~/.config/ryuzi/plugins/` are no longer
loaded (no installer provenance → skipped). To author a plugin:

1. **Catalog contribution** — add a TOML manifest under
   `crates/core/plugins/catalog/` and register it in `CATALOG_MANIFESTS`
   (see [Catalog contribution guidelines](#catalog-contribution-guidelines)).
2. **Skill pack** — publish a GitHub repo the skills installer
   understands (a `.codex-plugin/plugin.json` pack or a repo of leaf
   `SKILL.md` directories) and install it from Cockpit's Plugins hub (the
   header's **Add skill source** button, or a curated pack's Install button
   on the Browse tab); the installer materializes `ryuzi-plugin.toml` and
   the provenance stamp for you, subject to the same
   [two-phase trust gate](#two-phase-tiered-trust-gate) as any other
   skill-pack install.

   Minimal example — no `.codex-plugin/plugin.json`, just a `skills/`
   directory of leaf `SKILL.md` dirs:

   ```
   my-skills-repo/
   └── skills/
       └── code-review/
           └── SKILL.md
   ```

   Paste `owner/my-skills-repo` (or the full GitHub URL) into the
   **Add skill source** modal's source field. `discover_install_target` finds no
   `plugin.json` or top-level `SKILL.md`, scans `skills/*/SKILL.md`, and
   generates `~/.config/ryuzi/plugins/my-skills-repo/ryuzi-plugin.toml`
   with `id = "my-skills-repo"` and one `[[skills]]` entry
   (`path = "skills/code-review"`), plus the `.ryuzi-skill.json` provenance
   stamp — then materializes the skill to
   `~/.config/ryuzi/skills/my-skills-repo--code-review/`.

Then validate with the `plugin_detail` RPC (or the Cockpit plugin detail
screen) for `<id>`, enable it from Cockpit's Plugins screen (or the
`set_plugin_enabled` RPC), configure any `[auth]` credential, and start
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
  doc — the description field is what a user sees via the `plugin_detail`
  RPC and the Cockpit catalog card, so name the exact server/binary, any
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
  (including against every built-in provider/`native` id),
  requires every non-experimental entry to have `[[mcp]]`, and requires
  every `experimental` entry to have no `[[mcp]]`.

---

## Per-agent memory & learning

Every main agent (see [Agent delegation](setup.md#agent-delegation) in the
setup guide) keeps its own small, deterministic self-improvement loop:
persistent memory with a threat scan, cross-session recall via full-text
search, and a durable delivery queue that applies memory/skill/review/
journey writes into that agent's own OKF bundle. There is no shared global
memory store, no background "nudge"-driven review fork, and no autonomous
weekly curator daemon anymore — Plan 6 retired that shared subsystem
(`crates/core/src/curator.rs` and friends) in favor of per-agent state
scoped under `agents/<agent-id>/knowledge/`.

### Memory: scopes, budget, threat scan

`crates/core/src/harness/native/memory.rs` persists freeform text entries as
OKF concepts under the calling agent's own knowledge bundle (never inside a
session worktree, so memory writes can't dirty a feature branch):

- **`global`** — environment and conventions. Always available.
- **`user`** — who the user is (preferences, style, expectations). Always
  available.
- **`project`** — codebase-specific facts. Only available when a session has
  a project.

Each scope has a hard `6000`-character budget (`memory::BUDGET`); the
`memory` tool's guidance text (`MEMORY_GUIDANCE`, injected into the system
prompt alongside the scope snapshot every turn) tells the model to
consolidate rather than hoard once a scope nears that cap.

Because memory concepts are hand-editable, every entry is threat-scanned at
the point it's injected into the system prompt — never at write time, so the
concept on disk always reflects exactly what's there. A hit against the
prompt-injection pattern list (`memory::THREAT_PATTERNS` — "ignore all
previous", "system prompt", "you are now", `curl http://`, `<script`, etc.)
replaces the entry with `[BLOCKED: <reason> — edit this entry to restore
it]` in the injected snapshot only; a human can still open the raw concept
file (or use Cockpit's agent detail **Learning** tab, below) and fix or
remove the line.

### `session_search`: cross-session recall

The `session_search` native tool
(`crates/core/src/harness/native/tools/session_search.rs`) gives a session
DISCOVERY-style recall over past conversations, backed by the `messages_fts`
FTS5 index (see `Store::search_messages_fts`). It excludes
the calling session's own lineage (recall is for *past* sessions, not the
current thread) and excludes worker/review sessions outright, so
learning-loop chatter (nudge captures, review forks) never pollutes what a
normal chat session can recall. Results are capped at
`session_search::DISCOVERY_LIMIT` (12) hits, ranked by SQLite's FTS5
`snippet()`/`bm25` ordering.

### Durable Learning delivery queue

Memory writes, skill-usage observations, review notes, and journey
milestones are appended to `agent_learning_queue`
(`crates/core/src/agents/learning_queue.rs`) with a per-agent monotonic
sequence, and drained strictly in that order — one event per agent per
tick — by a daemon-hosted worker (`crates/core/src/learning.rs`, polling
every 5s) that applies each event into the target agent's OKF bundle.
Application is idempotent — every produced concept records the event id in
frontmatter, and a replay that finds the id already recorded is a no-op —
so the crash window between apply and acknowledge can never duplicate
knowledge. A stuck head-of-line event blocks only its own agent (until its
claim goes stale and is reclaimed); every other agent's queue keeps
draining.

### Cockpit agent detail: the Learning tab

The old global Learning sidebar/panel is gone. Its replacement lives on
each agent's own detail screen (`AgentDetailView.tsx`'s **Learning** tab,
`AgentLearningTab.tsx`), backed by the `get_agent_learning` RPC
(`crates/core/src/api/agent_api.rs`) — everything shown is scoped to that
one agent's own `agents/<agent-id>/knowledge/` bundle:

- **Memory** — every memory concept for this agent, with inline add/edit/
  delete (`create_agent_concept`/`update_agent_concept`/
  `delete_agent_concept`).
- **Journey** — milestone concepts rendered as a timeline
  (`JourneyGraph`).
- **Skill usage** — per-skill use/success counters recorded from this
  agent's own tool calls.
- **Reviews** — retrospective/finding concepts (`ReviewFeed`).
- **Curator** — a forward-looking placeholder panel that would list
  consolidated-state snapshots (`ConceptArea::CuratorHistory`) and offer a
  **Restore snapshot** action (`CuratorCard`) enqueuing a `Rollback`
  learning event through the same durable queue above. There is no
  autonomous background sweep anymore, and — importantly — no in-product
  path creates the first snapshot today: the concept-CRUD RPCs only ever
  write `Memory` concepts, and the restore path requires an already-present
  `curator/history/<id>.md` to restore from. So this panel stays empty until
  a snapshot producer ships; treat it as reserved UI, not a working feature.
- **Repair knowledge** — any OKF concept file that failed to parse/validate
  is listed with its error, editable in place (raw Markdown, validate, then
  replace) or deletable, via `delete_invalid_agent_concept`.

## Remote catalog

The embedded catalog above ships inside the binary — adding or fixing an
integration otherwise needs a full release. The remote catalog lets the
daemon fetch a **signed** `catalog.json` feed at runtime and merge it over
the embedded set, so new/updated integrations (and revocations) can ship
between releases. Engine code: `crates/core/src/plugins/remote_catalog.rs`
(fetch/verify/cache + the background cadence), `catalog_feed_key.rs` (the
embedded public key), `catalog.rs`'s `merged_catalog_plugins` (the
version-gated merge), and the catalog cache tables in `store.rs`.
Publish tooling: `scripts/catalog/*.ts` (this section's second half).

### Feed format

A feed is two files served side by side: `catalog.json` (the payload) and
`catalog.json.sig` (a **raw, detached** 64-byte ed25519 signature over the
exact bytes of `catalog.json` — not base64, not a PEM/DER wrapper, not a
signature over a hash). `CatalogFeed` (`remote_catalog.rs`) deserializes:

| Field | Type | Notes |
| --- | --- | --- |
| `schemaVersion` | integer | Must be `1` — any other value is rejected outright (`CatalogFeedError::UnsupportedSchema`). |
| `sequence` | integer | Monotonic publish counter. A fetched feed whose `sequence` is **strictly less than** the last accepted one is rejected (`CatalogFeedError::Rollback`) — this is the anti-rollback/anti-replay guard; an equal or greater sequence is accepted (re-applying an unchanged feed is a no-op, not an error). Persisted in the `catalog_feed_state` table. |
| `generatedAt` | integer | Epoch milliseconds; informational only (not checked). |
| `entries` | `{id, manifestToml}[]` | One embedded-or-new catalog manifest per entry, as raw TOML text (the same format as `crates/core/plugins/catalog/*.toml`). `id` **must** equal the manifest TOML's own `id` field — the engine's merge (`merged_catalog_plugins`) rejects and logs (never applies) an entry whose declared feed id and manifest id disagree, to avoid overwriting the wrong embedded slot. |
| `blocked` | `{id, reason, sinceSequence}[]` | A denylist of ids to revoke — see [The `blocked` denylist](#the-blocked-denylist) below. |

### Signing

The feed is verified with `CATALOG_FEED_PUBKEY`, a 32-byte ed25519 public
key **compiled into the binary**
(`crates/core/src/plugins/catalog_feed_key.rs`). The matching private key
never ships anywhere — it lives only as the `CATALOG_FEED_PRIVATE_KEY` CI
secret, consumed by the publish tooling below.

`CATALOG_FEED_PUBKEY` currently ships as the **all-zero placeholder**
(`[0u8; 32]`). That key is a valid *low-order* ed25519 point — a non-strict
verify could be tricked into accepting a forged signature against it — so the
engine rejects it two ways: an explicit all-zero guard **plus** `verify_strict`
(which rejects low-order keys and non-canonical signatures). While the
placeholder is in place **every fetch is rejected**
(`CatalogFeedError::BadSignature`); the remote catalog is fail-closed and the
embedded catalog still loads normally. Going live is a one-time human ops step:

1. Run `bun scripts/catalog/keygen.ts` **once** (a second run makes an
   unrelated keypair, not a recovery of the first). It prints:
   - a Rust `[u8; 32]` array literal — the **public** key, safe to commit.
   - a base64 string — the **private** key seed. Never commit it.
2. Store the private key as the `CATALOG_FEED_PRIVATE_KEY` repo secret
   (GitHub Settings → Secrets and variables → Actions).
3. Paste the public key array into `CATALOG_FEED_PUBKEY`
   (`catalog_feed_key.rs`) and ship that change in a normal PR.
4. The next release's `catalog-feed` job (see
   [Publish flow](#publish-flow) below) builds and uploads a feed the new
   binary — carrying the new pubkey — can verify. Older, already-shipped
   binaries keep verifying against whatever pubkey they were built with, so
   rotating the key later means republishing a fresh signed feed once the
   new pubkey has actually shipped, or older installs simply stop accepting
   updates until they upgrade.

Bun's WebCrypto (`crypto.subtle`, algorithm `"Ed25519"`) is what both
`keygen.ts` and `build-feed.ts` use — no external signing dependency.
Verified byte-for-byte interoperable with `ed25519-dalek` (the crate the
engine verifies with): signing the same 32-byte seed and message with Bun's
WebCrypto and with `ed25519_dalek::SigningKey::sign` produces the identical
64-byte signature.

### Fetch pipeline

| Setting | Default | Notes |
| --- | --- | --- |
| `catalog_feed_url` | `https://github.com/alfin-efendy/ryuzi/releases/latest/download/catalog.json` (`DEFAULT_CATALOG_FEED_URL`) | Override for a self-hosted feed. The `.sig` is always fetched from `<feed_url>.sig`. |
| `catalog_fetch_interval_ms` | `21600000` (6h, `DEFAULT_CATALOG_FETCH_INTERVAL_MS`) | Background fetch cadence. |

Both are plain settings-store keys, but neither is in the static
`ConfigField` schema (`crates/core/src/settings/fields.rs`) — there's no
Settings-screen form field for them yet. Set them via the `set_setting` RPC
(Cockpit's generic `set_setting` Tauri command, or `POST /rpc/set_setting`
with `{"key": "catalog_feed_url", "value": "..."}`) — that RPC writes
through `Store::set_setting` directly, which (unlike the schema-validated
`SettingsStore::set` most other settings go through) accepts any key, so an
unregistered key like these two still persists. Read them back with
`get_setting`.

`RemoteCatalogManager` (`remote_catalog.rs`) owns the background cadence:
fetch once on boot, then on a `catalog_fetch_interval_ms` timer, mirroring
`UpdateManager`'s shape. It is wired into **`ryuzi serve` / `ryuzi __daemon`
only** (`crates/cli/src/daemon_cmd.rs`) — Cockpit's own spawned
`--engine-daemon` subprocess (`apps/cockpit/src-tauri/src/engine_daemon.rs`)
does not start this timer. Cockpit still sees a live feed via two other
paths: (a) if Cockpit attaches to an *already-running* `ryuzi serve`
daemon (`connect_or_spawn` in `apps/cockpit/src-tauri/src/engine.rs` prefers
an existing daemon over spawning its own), that daemon's timer is the one
driving it; (b) either way, every daemon composition root
(`daemon::build_daemon`) merges whatever is *already cached* in the shared
SQLite store at startup, and exposes the `refresh_catalog` RPC (below) for
an on-demand fetch regardless of which daemon is running.

Every applied fetch (background or on-demand) re-runs the
[blocked denylist sweep](#the-blocked-denylist) and, only if the
*effective* merged catalog actually changed (new/removed/version-changed/
blocked entries — not just a re-stamped `fetched_at` or an unchanged
re-fetch), sets the daemon's in-memory `plugins_restart_required` flag —
the same flag skill-pack installs/updates set, surfaced the same way (see
[Daemon RPC methods](#daemon-rpc-methods-post-rpcmethod) above).

### Verification and anti-rollback

`fetch_and_cache` (`remote_catalog.rs`) never propagates a failure as an
error — every failure path (non-2xx HTTP, bad signature, unsupported
schema, rollback, unparsable entry) is caught, logged
(`tracing::warn!`), and returns `FetchOutcome { applied: false, .. }`, so a
transient network blip or a compromised/misconfigured feed can never crash
the daemon or the background timer. An entry whose TOML fails to
parse/validate is dropped individually (the rest of the feed still
applies); an entry whose declared `id` doesn't match its manifest's own
`id` is dropped the same way (see [Feed format](#feed-format) above).

### Version-gated merge

`merged_catalog_plugins` (`catalog.rs`) starts from the embedded catalog and,
for every cached **non-blocked** remote row: parses+validates its manifest,
rejects an id/manifest-id mismatch, then — keyed on the manifest's own
`id` — either appends it (a new id) or replaces the embedded entry **only
if** the remote manifest's semver is strictly greater (`semver_gt`; an
unparsable version never wins, so the embedded entry survives). An embedded
entry is never deleted, only shadowed by a higher-version override. This
merge only runs in the daemon composition root
(`daemon::build_daemon`) — the plain CLI (`ryuzi plugins list`, `ryuzi
config`, ...) uses the embedded-only `install_builtins`/`catalog_plugins`
path and never sees remote entries.

### The `blocked` denylist

A feed's `blocked` array revokes ids — including ids that were never in the
embedded catalog at all (a purely remote entry can be blocked too). A
blocked row:

- Is **excluded entirely** from `merged_catalog_plugins`, even at an
  absurdly high version — it can never override (or, if new, add) a plugin.
- Makes `toggle_enabled` **refuse** to enable that id going forward
  (`"blocked by catalog: {reason}"` — checked via `plugins::is_blocked`).
- Gets **live auto-disabled** if it was already enabled: every applied
  fetch runs `apply_blocked_denylist`, which force-sets
  `plugin.<id>.enabled=false` for any currently-enabled blocked id (logged,
  best-effort — a settings-write failure is logged and does not fail the
  fetch).
- Surfaces as an `error`-severity `"blocked"` finding in `plugin_doctor`
  (`{id} was revoked by the catalog` / "Uninstall or stop using {id}") and
  a `BlockedBadge` in Cockpit's Browse/Installed cards, independent of
  whether the auto-disable sweep has already run.

### Daemon RPC

Two params-free methods, alongside the plugin RPC family (see
[Daemon RPC methods](#daemon-rpc-methods-post-rpcmethod) above):

| Method | Result | Notes |
| --- | --- | --- |
| `refresh_catalog` | `CatalogStatus` | Fetches, verifies, and caches the feed **right now** instead of waiting for the background cadence — the only way to get a fresh feed on a daemon with no timer (Cockpit's own `--engine-daemon`, see [Fetch pipeline](#fetch-pipeline)). Re-runs the blocked-denylist sweep and sets `plugins_restart_required` on an effective change, same as the background cadence. |
| `catalog_status` | `CatalogStatus` | Read-only snapshot: no fetch. |

`CatalogStatus`: `{ sequence, lastFetchAt, outcome, entries, blocked }` — the
last accepted feed's sequence/fetch-time/outcome (`"ok"` or a failure
message) plus cached non-blocked/blocked row counts. Cockpit's Browse tab
renders this as a status line (`catalogStatusLabel` in `PluginsView.tsx`,
e.g. `"Catalog seq 9 · 24 entries, 1 blocked · fetched 7/11/2026, 3:04 PM"`)
next to a **Refresh catalog** button that calls `refresh_catalog` and toasts
the result.

### Publish flow

`scripts/catalog/build-feed.ts` (Bun) builds and signs a feed:

1. Reads every `crates/core/plugins/catalog/*.toml`, deriving each entry's
   `id` from the manifest's own `id` field (never the filename) — this is
   what guarantees entries can never fail the engine's id/manifest-id
   mismatch check.
2. Reads the optional `scripts/catalog/blocklist.json` — a JSON array of
   `{"id": "...", "reason": "...", "sinceSequence"?: <int>}` — `[]` if the
   file doesn't exist. `sinceSequence` defaults to the sequence being built
   when omitted.
3. Reads the current value from `scripts/catalog/sequence.txt` (`0` if the
   file doesn't exist yet), increments it by one, and uses that as the
   feed's `sequence` — then writes the incremented value back to
   `sequence.txt` for the next run.
4. Serializes the feed to JSON **exactly once**, signs those exact bytes
   with the base64-seed private key from the `CATALOG_FEED_PRIVATE_KEY` env
   var, and writes both the signed bytes (`catalog.json`) and the raw
   64-byte signature (`catalog.json.sig`) — never re-serializing between
   signing and writing, since the engine verifies the signature over the
   exact downloaded bytes.

Run it locally with a keypair from `keygen.ts`:

```sh
bun scripts/catalog/keygen.ts                        # once, to get a keypair
export CATALOG_FEED_PRIVATE_KEY=<the printed base64>  # never commit this
bun scripts/catalog/build-feed.ts
# -> wrote catalog.json + catalog.json.sig — sequence N, 24 entries, 0 blocked
```

`catalog.json`/`catalog.json.sig` written at the repo root are gitignored
(`/catalog.json`, `/catalog.json.sig` in `.gitignore`) — they're a release
artifact, not tracked source. `scripts/catalog/sequence.txt` **is** tracked
(it's the persisted publish counter); `scripts/catalog/blocklist.json` is
optional and untracked unless a maintainer adds one.

`scripts/catalog/ed25519.ts` holds the shared WebCrypto helpers (keypair
generation, PKCS8⇄raw-seed conversion, sign, verify);
`scripts/catalog/build-feed.test.ts` is a `bun test` round trip — sign a
fixture feed with a freshly generated keypair, verify it with that
keypair's public key, and assert a single tampered byte or an unrelated
keypair fails verification.

The release workflow's `catalog-feed` job (`.github/workflows/release.yml`)
runs on every CLI release: guarded on the `CATALOG_FEED_PRIVATE_KEY` secret
being present (a no-op, not a failure, when it isn't — mirroring
`cockpit-release.yml`'s optional Apple codesigning), it runs
`bun scripts/catalog/build-feed.ts` and uploads `catalog.json` +
`catalog.json.sig` onto the release via `gh release upload`, matching the
asset-upload pattern the `cockpit-release.yml` `publish` job and the
`docker`/`npm` jobs already use.

**Known limitation:** the workflow does not commit the incremented
`scripts/catalog/sequence.txt` back to `main` — like `scripts/npm/
set-version.ts`'s version stamps, the increment is local to that CI run and
discarded with the runner. Until a maintainer bumps and commits
`sequence.txt` by hand (or a follow-up automates that commit), every
release's feed reuses the same `sequence`, which the anti-rollback check
still accepts (only a *lower* sequence is rejected) but does not usefully
track feed freshness/generation across releases the way a truly
monotonically-advancing counter would.
