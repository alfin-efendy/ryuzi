# Plugins Hub Design

Date: 2026-07-07

## Goal

Cockpit should present one coherent **Plugins** hub for extension management. It should replace the current Apps-first navigation, merge the embedded Ryuzi plugin catalog into registry browsing, remove duplicate MCP registry versions, make OAuth-only catalog plugins usable through a Cockpit OAuth broker, and install Ryuzi-native skills from GitHub or skills.sh-style sources, including plugin skill packs such as `obra/superpowers`.

## Decisions

- Use a single sidebar entry named **Plugins**.
- Build a vertical slice across UI, registry grouping, plugin OAuth, and skills install instead of doing a UI-only rename.
- Store installed skills in Ryuzi-native locations, not global Codex or Claude locations.
- Use a hybrid OAuth broker: generic MCP OAuth discovery first, with manifest overrides for providers that need custom endpoints, scopes, or client settings.
- Keep installed MCP server persistence in the existing Apps/MCP store. Rename the user-facing surface without forcing a DB concept rename.

## Current Context

The current Cockpit app separates three related concepts:

- `AppsView` lists installed MCP servers and has a `Catalog` tab for embedded Ryuzi plugin manifests.
- `RegistryView` searches the official MCP registry and installs entries into Apps.
- `PluginDetailView` configures Ryuzi plugin manifests, but OAuth-only plugins currently show "Sign-in for this plugin isn't wired in Cockpit yet".

The Rust engine already has useful foundations:

- `PluginManifest` supports metadata, auth, settings, MCP servers, and bundled skills.
- `PluginHost::enabled_skill_dirs` exposes enabled user plugin skills to native sessions.
- `registry_cmd.rs` maps official MCP registry entries but does not collapse multiple versions.
- Provider OAuth flows already exist for model connections and can inform the plugin OAuth UX, but plugin OAuth needs its own generic MCP-oriented flow.

The official MCP registry returns multiple records for the same server name across versions and marks the latest record through metadata. The MCP authorization spec requires remote HTTP MCP clients to support OAuth protected resource discovery. The skills.sh docs expose public install usage through `npx skills add <source>`, while its search/detail API requires Vercel OIDC authentication, so v1 should support direct install and curated shortcuts rather than in-app live skills search.

## UX Design

The sidebar item **Apps** becomes **Plugins**. The nav group should cover the hub, installed MCP server detail, Ryuzi plugin detail, and browse states.

The existing plugin-driven sidebar section should be renamed **Enabled plugins** so it does not visually conflict with the main Plugins navigation item.

The Plugins hub has four tabs:

1. **Installed**
   - Shows installed MCP servers from the existing Apps store.
   - Keeps the current cards, status, scope display, and configure action.

2. **Access**
   - Keeps the current agent/runtime access matrix for installed MCP servers.
   - Continues to operate on existing MCP server rows and permissions.

3. **Browse**
   - Replaces both the old Apps -> Catalog tab and standalone Registry view.
   - Provides source filters for Ryuzi catalog and MCP registry.
   - Ryuzi catalog cards open `PluginDetailView`.
   - MCP registry cards install into Installed.
   - Multiple registry versions collapse into one card with a latest badge. If the group contains more than one version, show a version selector defaulted to the selected latest version.

4. **Skills**
   - Supports curated installs such as Superpowers.
   - Supports direct install from `owner/repo`, GitHub URL, skills.sh URL that resolves to a GitHub-backed source, or another compatible GitHub repo URL.
   - Lists installed Ryuzi-native skills and skill packs with update/remove actions.

## Backend And Data Flow

Installed MCP servers remain backed by the existing MCP server store:

- Keep `apps_cmd.rs`, `useApps`, and `mcp::*` as the source of truth for installed MCP servers.
- Rename visible UI language from Apps to Plugins where appropriate.
- Avoid a database migration unless a later implementation needs persisted UI labels.

The Plugins hub composes multiple existing and new commands:

- `list_plugins` and `plugin_detail` for embedded and user Ryuzi plugins.
- `registry_search` for official MCP registry browsing.
- New skills commands:
  - `list_skills`
  - `install_skill_from_source`
  - `remove_skill`
  - `refresh_skill` to reinstall a previously installed skill or skill pack from its recorded source.

Registry dedupe should happen in Rust:

- Group registry records by canonical `server.name`.
- Preserve all versions in a `versions` list.
- Select the record with metadata `isLatest == true` as the default when present.
- Fall back to highest semver-like version when `isLatest` is absent.
- Keep install target, website, publisher, transport kind, and description for the selected version.

## Plugin OAuth Broker

OAuth support should target HTTP MCP plugins declared through `PluginManifest`.

The manifest auth block can be extended with optional OAuth fields:

- `authorize_url`
- `token_url`
- `resource`
- `scopes`
- `client_id_setting`
- `client_secret_setting`
- `dynamic_registration`
- `extra_authorize_params`
- `extra_token_params`

Generic flow:

1. `begin_plugin_oauth(plugin_id)` reads the plugin detail and chooses its HTTP MCP endpoint.
2. Cockpit probes the endpoint and follows MCP protected resource metadata discovery.
3. The backend discovers authorization and token endpoints.
4. The backend attempts dynamic client registration when the server supports it.
5. If registration is not available, Cockpit asks for client id and optional client secret using manifest setting keys.
6. The backend opens the browser with PKCE and resource/scope parameters.
7. `complete_plugin_oauth(...)` stores token material securely in the Ryuzi settings/store layer.

Stored token state should include:

- access token
- refresh token
- expiry
- scope
- token type
- discovered auth server metadata needed for refresh
- reconnect-needed flag

Connector behavior:

- `ensure_auth` treats missing OAuth token as a configuration error with a clear Cockpit action.
- When building HTTP `McpServerSpec`, the connector injects `Authorization: Bearer <access_token>`.
- If a token is near expiry and a refresh token exists, refresh before attaching.
- If refresh fails, mark reconnect required, skip the plugin for that session, and report a friendly configure/reconnect message.

Hybrid behavior:

- Generic discovery is the default.
- Catalog manifests may override auth URLs, scopes, resource, or client setting keys.
- Catalog manifests may add extra authorization or token request parameters through `extra_authorize_params` and `extra_token_params`.
- OAuth-only plugins should not show the old "Sign-in isn't wired" message once this broker exists.

## Skills Install

Skills install is Ryuzi-native.

Single skill storage:

- Copy a valid skill folder to `~/.config/ryuzi/skills/<skill-name>`.
- A valid single skill contains `SKILL.md`.
- Keep provenance metadata in a small Ryuzi-managed registry file so reinstall/update can show source and version/hash when available.

Plugin skill pack storage:

- Copy plugin packs to `~/.config/ryuzi/plugins/<pack-id>/`.
- Generate or maintain `ryuzi-plugin.toml` with `[[skills]]` entries pointing at the copied skill directories.
- Enable the generated user plugin by setting `plugin.<pack-id>.enabled = true`.

Superpowers support:

- Resolve `obra/superpowers`.
- Read `.codex-plugin/plugin.json`.
- Use its `skills` path, currently `./skills/`.
- Copy the skills folder into a Ryuzi user plugin directory.
- Generate a manifest with identity, description, homepage, publisher, and `[[skills]]` entries.
- Enable the plugin after install so native sessions discover the skills through `PluginHost::enabled_skill_dirs`.

Validation:

- Reject sources with no discoverable skills.
- Reject paths escaping the install directory.
- Reject oversized or unexpected file trees.
- Preserve existing working installs on failed update.
- Clean temp directories after success or failure.

## Error Handling

- Registry unreachable: show an inline Browse error without breaking Installed or Access tabs.
- Duplicate registry versions: never show multiple cards for the same canonical server name.
- OAuth discovery failure: explain whether protected resource metadata, auth server metadata, dynamic registration, or client credentials are missing.
- OAuth refresh failure: mark the plugin as reconnect required and skip attaching it instead of failing the session.
- Skill install failure: leave the previous installed version intact and show the failing validation reason.

## Testing Plan

Frontend:

- Update sidebar/nav tests for the visible Plugins label and route grouping.
- Test Browse source filters.
- Test registry grouped-version rendering and selected/latest install action.
- Test Skills direct input validation states.
- Rename or replace AppsView tests with PluginsView coverage while preserving existing Installed and Access behavior.

Rust:

- `registry_cmd.rs` grouped mapping tests:
  - multiple versions collapse into one entry
  - `isLatest` wins
  - semver-ish fallback works
  - selected install target is preserved
- OAuth broker pure tests:
  - parse `WWW-Authenticate` resource metadata
  - discover protected resource metadata
  - apply manifest override precedence
  - decide refresh-before-expiry
  - inject bearer headers into HTTP MCP specs
- Skills installer tests:
  - single skill install from temp source
  - skill pack install from `.codex-plugin/plugin.json`
  - Superpowers-like fixture
  - missing `SKILL.md`
  - path traversal attempt
  - reinstall preserves previous version on failure

Minimum verification for implementation:

- `cargo test -p ryuzi-core`
- `cargo test -p ryuzi-cockpit`
- targeted `bun test apps/cockpit/src/...`
- `bun run --cwd apps/cockpit build`

## Out Of Scope For V1

- Live skills.sh search inside Cockpit, because the documented API requires Vercel OIDC authentication.
- Installing skills into global Codex or Claude directories.
- Renaming the underlying MCP server database tables from Apps to Plugins.
- Full provider-specific OAuth implementation for every vendor beyond generic discovery plus manifest overrides needed by the catalog entries touched in this slice.
