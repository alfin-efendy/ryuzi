# WASM Component Plugin Platform Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Deliver signed, independently versioned WASI Component Model plugins starting at ABI `0.1.0`, then migrate first-party providers and integrations without Discord- or provider-ID-specific runtime wiring.

**Architecture:** The work is deliberately split into independently shippable phases. The first phase establishes bundle contracts, trust, storage, generic discovery, and installer behavior; the second adds the WIT host and generic adapters; the final phases port pilots one capability family at a time. `ryuzi-plugin-sdk` owns serializable contracts and `ryuzi-core` owns all execution, secrets, policy, SQLite persistence, daemon lifecycle, and Cockpit RPCs.

**Tech Stack:** Rust 2021, Cargo workspace, `wasmtime` Component Model + WASI Preview 2, WIT, `wit-bindgen`, `serde`, `semver`, `sha2`, `ed25519-dalek`, SQLite (`rusqlite`/`deadpool-sqlite`), Tokio, Reqwest, Tauri 2, React, Bun.

## Global Constraints

- WIT package and world versions start at `0.1.0`; components declare an accepted host range such as `>=0.1.0, <0.2.0`.
- In `0.x`, breaking ABI changes increment the minor version and compatible changes increment the patch version; ABI reaches `1.0.0` only after pilot stability.
- All installable code is a signed first-party bundle. Verify catalog trust, release signature/key ID, SHA-256, manifest, WIT range, component imports/exports, and requested permissions before activation.
- Store releases at `plugins/<id>/<version>/`; activation is an atomic active-version-pointer change. Never activate an invalid, incompatible, or revoked bundle.
- Plugins receive no raw OAuth access/refresh token, raw socket, unrestricted network, arbitrary filesystem, environment, or subprocess capability.
- HTTP targets and every redirect must match the manifest allowlist. Plugin state and settings are namespaced by plugin ID.
- Long-lived gateway/provider components are supervised; connector/hook calls are stateless. Traps, limit breaches, and invalid output must never crash the daemon; `tool.before` hooks fail open.
- Bootstrap only `mimo` and `opencode`; bootstrap failure must not fail application installation and must be retryable.
- No production branch on a plugin ID. Specifically remove all Discord-specific gateway factory, defaults, registry wiring, and global settings after its WASM replacement is proven.
- Use TDD, `cargo fmt`, targeted Rust tests, and the UI build/test matrix in `AGENTS.md`. Do not hand-edit generated Cockpit bindings.

---

## Delivery map

| Phase | Independently testable deliverable | Blocks |
| --- | --- | --- |
| 1 | Signed component-bundle installer and versioned install ledger; no component execution yet | All later phases |
| 2 | WIT `0.1.0` packages plus a generic, capability-denying Component host | All component execution |
| 3 | Scoped settings/storage/HTTP/OAuth host imports with policy tests | First-party networked plugins |
| 4 | Generic connector, hook, provider, and gateway adapters with fixture components | Pilot migrations |
| 5 | Mimo/OpenCode bootstrap provider bundles and generic provider routing | First-run model availability |
| 6 | GitHub 0.1 connector and OAuth/approval pilot | OAuth/tool proof |
| 7 | Discord gateway migration and deletion of native hardcode | Generic long-lived gateway proof |
| 8 | Atlassian and Bitbucket connector bundles | Product pilots |
| 9 | Remaining providers, legacy cleanup, Cockpit polish, and release verification | Completion criteria |

Each phase is a review and release boundary. Do not begin a phase until its predecessor's listed verification passes.

## File structure introduced by the program

| Path | Responsibility |
| --- | --- |
| `crates/plugin-sdk/src/bundle.rs` | Serializable bundle/release/permission/ABI contract and structural validation |
| `crates/plugin-sdk/wit/ryuzi-plugin.wit` | Root `ryuzi:plugin@0.1.0` WIT package/world |
| `crates/plugin-sdk/wit/ryuzi-*.wit` | Versioned WIT host capability packages |
| `crates/core/src/plugins/bundle.rs` | Staging, hash/signature verification, atomic activation, local release discovery |
| `crates/core/src/plugins/runtime.rs` | Wasmtime engine configuration, component validation, limits, instance supervision |
| `crates/core/src/plugins/capabilities/*.rs` | One generic host adapter per WIT capability |
| `crates/core/src/plugins/wasm_*.rs` | Generic `Connector`, hook, provider, and gateway bridge implementations |
| `crates/core/src/store.rs` | New migrations and CRUD for installed component releases/profiles/status |
| `crates/core/src/api/plugins_api.rs` | RPCs/DTO mapping for component installation, permission review, lifecycle, and OAuth profiles |
| `apps/cockpit/src/views/PluginsView.tsx` | Catalog/installed bundle state and permission/install UX |
| `apps/cockpit/src/views/PluginDetailView.tsx` | Version, health, pin/rollback, policy, and per-profile connection UI |
| `plugins/<id>/` | First-party source bundle projects; each builds its own component/release artifact |
| `scripts/plugins/` | Reproducible first-party component build, manifest, hashing, signing, and catalog publication tooling |

## Phase 1 — Bundle contract, trust, and persistent installation

### Task 1: Add the SDK bundle and permission contract

**Files:**
- Create: `crates/plugin-sdk/src/bundle.rs`
- Modify: `crates/plugin-sdk/src/lib.rs`
- Modify: `crates/plugin-sdk/Cargo.toml`
- Test: inline `#[cfg(test)]` module in `crates/plugin-sdk/src/bundle.rs`

**Interfaces:**
- Produces `PluginBundleManifest`, `PluginRelease`, `PluginPermissions`, `NetworkPermission`, `OAuthProfile`, `PluginLifecycle`, and `BundleError`.
- Produces `PluginBundleManifest::from_toml(&str) -> Result<Self, BundleError>` and `PluginRelease::from_json(&[u8]) -> Result<Self, BundleError>`.

- [ ] **Step 1: Write failing parser and validation tests.** Cover a valid `0.1.0` connector bundle, rejected non-semver release, rejected ABI range that cannot parse, duplicate OAuth profile IDs, an HTTP URL in the allowlist, and a wildcard that is not a suffix-only DNS pattern.

```rust
#[test]
fn bundle_manifest_rejects_network_path_and_duplicate_oauth_profile() {
    let err = PluginBundleManifest::from_toml(r#"
contract = 1
id = "github"
name = "GitHub"
version = "0.1.0"
wit-api = ">=0.1.0, <0.2.0"
lifecycle = "stateless"
[permissions]
network = ["https://api.github.com/v3", "*.github.com"]
[[oauth-profile]]
id = "github"
[[oauth-profile]]
id = "github"
"#).unwrap_err();
    assert!(matches!(err, BundleError::InvalidNetworkDomain(_) | BundleError::DuplicateOauthProfile(_)));
}
```

- [ ] **Step 2: Run the test and confirm it fails because the contract module does not exist.**

Run: `cargo test -p ryuzi-plugin-sdk bundle_manifest_rejects_network_path_and_duplicate_oauth_profile`

Expected: compilation failure mentioning `bundle` or `PluginBundleManifest`.

- [ ] **Step 3: Implement the contract with serde-only data types and validation.** Use `semver::Version` and `semver::VersionReq`; move both dependencies to `[workspace.dependencies]`, add them with `workspace = true` in the SDK, and keep validation structural. Require `id`, `name`, `version`, `wit-api`, `lifecycle`, a non-empty component filename, and unique OAuth profile IDs. Network entries must be lowercase hostnames or `*.` hostname suffixes—no scheme, path, port, IP literal, or bare `*`.

```rust
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub struct PluginBundleManifest {
    pub contract: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub wit_api: String,
    pub lifecycle: PluginLifecycle,
    pub component: String,
    #[serde(default)] pub permissions: PluginPermissions,
    #[serde(default, rename = "oauth-profile")] pub oauth_profiles: Vec<OAuthProfile>,
}

impl PluginBundleManifest {
    pub fn from_toml(input: &str) -> Result<Self, BundleError> {
        let manifest: Self = toml::from_str(input)?;
        manifest.validate()?;
        Ok(manifest)
    }
    pub fn validate(&self) -> Result<(), BundleError> { /* exact checks above */ }
}
```

- [ ] **Step 4: Export the contract and run all SDK tests.**

Run: `cargo test -p ryuzi-plugin-sdk`

Expected: PASS.

- [ ] **Step 5: Commit the isolated SDK contract.**

```bash
git add Cargo.toml crates/plugin-sdk/Cargo.toml crates/plugin-sdk/src/lib.rs crates/plugin-sdk/src/bundle.rs Cargo.lock
git commit -m "feat(plugin-sdk): add component bundle contract"
```

### Task 2: Add signed release metadata and local artifact verification

**Files:**
- Modify: `crates/plugin-sdk/src/bundle.rs`
- Create: `crates/core/src/plugins/bundle.rs`
- Modify: `crates/core/src/plugins/mod.rs`
- Test: inline tests in `crates/core/src/plugins/bundle.rs`

**Interfaces:**
- Consumes `PluginBundleManifest`, `PluginRelease`.
- Produces `VerifiedBundle { manifest, release, staging_dir }` and `verify_bundle(staging_dir, trusted_keys) -> anyhow::Result<VerifiedBundle>`.

- [ ] **Step 1: Write failing tests using a generated `ed25519_dalek::SigningKey`.** Build a temp staging directory containing valid `ryuzi-plugin.toml`, `plugin.wasm` bytes, `release.json`, and `plugin.sig`; prove valid hash/signature passes, altered WASM fails with a hash error, and a signature from an untrusted key fails before activation.
- [ ] **Step 2: Run the focused core test.**

Run: `cargo test -p ryuzi-core plugins::bundle::tests::rejects_tampered_artifact`

Expected: FAIL because `plugins::bundle` is absent.

- [ ] **Step 3: Implement verification in this order:** parse the release JSON; parse and validate the manifest; require release ID/version to equal the manifest; SHA-256 the component bytes and compare a lowercase hex digest with release metadata; verify detached Ed25519 signature over canonical `release.json` bytes; reject unknown key IDs; canonicalize paths and require the component remains beneath staging.
- [ ] **Step 4: Run `cargo test -p ryuzi-core plugins::bundle` and `cargo fmt`.**
- [ ] **Step 5: Commit.**

```bash
git add crates/core/src/plugins/bundle.rs crates/core/src/plugins/mod.rs crates/plugin-sdk/src/bundle.rs
git commit -m "feat(core): verify signed plugin bundles"
```

### Task 3: Persist independently versioned component releases and active versions

**Files:**
- Modify: `crates/core/src/store.rs`
- Test: inline store migration/CRUD tests in `crates/core/src/store.rs`

**Interfaces:**
- Produces `ComponentPluginReleaseRecord { plugin_id, version, source_url, sha256, signing_key_id, installed_at, active, revoked, revocation_reason }`.
- Produces store methods `upsert_component_release`, `list_component_releases`, `active_component_release`, `set_active_component_release`, and `mark_component_release_revoked`.

- [ ] **Step 1: Write failing migration and CRUD tests.** Assert two versions of `github` can coexist, exactly one can be active, setting `0.1.1` active clears `0.1.0`, and revoking an active version clears active status with the supplied reason.
- [ ] **Step 2: Run the selected test and confirm migration APIs are missing.**

Run: `cargo test -p ryuzi-core component_release_activation_is_exclusive`

Expected: compilation failure for `ComponentPluginReleaseRecord`.

- [ ] **Step 3: Add a new tail migration—never edit existing migration slots.** Create `component_plugin_releases(plugin_id, version, source_url, sha256, signing_key_id, installed_at, active, revoked, revocation_reason, PRIMARY KEY(plugin_id, version))`; use a transaction in `set_active_component_release` to clear then set active and reject absent/revoked target versions.
- [ ] **Step 4: Run focused store tests, then `cargo test -p ryuzi-core store::tests`.**
- [ ] **Step 5: Commit.**

```bash
git add crates/core/src/store.rs
git commit -m "feat(store): track versioned component plugin releases"
```

### Task 4: Atomically activate verified releases and discover installed bundles

**Files:**
- Modify: `crates/core/src/plugins/bundle.rs`
- Modify: `crates/core/src/plugins/mod.rs`
- Test: inline tests in `crates/core/src/plugins/bundle.rs`

**Interfaces:**
- Produces `ComponentBundleInstaller::install_verified(&self, bundle: VerifiedBundle) -> anyhow::Result<ComponentPluginReleaseRecord>`.
- Produces `installed_bundle_root() -> PathBuf` and `load_active_bundles(root, store) -> Vec<InstalledBundle>`.

- [ ] **Step 1: Write failing filesystem tests.** Assert installation ends at `<temp>/github/0.1.0`, no staging directory remains, the active pointer resolves `0.1.0`, and an injected failure before pointer replacement leaves the old active pointer and DB active release unchanged.
- [ ] **Step 2: Run the focused installer test.**

Run: `cargo test -p ryuzi-core plugins::bundle::tests::failed_activation_preserves_previous_release`

Expected: FAIL until `ComponentBundleInstaller` exists.

- [ ] **Step 3: Implement an atomic staging-to-version move and pointer update.** Use a sibling temporary pointer file followed by atomic rename. On Windows, ensure existing pointer replacement is performed with the repository's existing atomic-file convention rather than deleting the active pointer first. Commit DB state only after filesystem activation succeeds; on DB failure restore the previous pointer.
- [ ] **Step 4: Run all bundle tests and `cargo fmt`.**
- [ ] **Step 5: Commit.**

```bash
git add crates/core/src/plugins/bundle.rs crates/core/src/plugins/mod.rs
git commit -m "feat(core): atomically activate component bundles"
```

## Phase 2 — WIT ABI and capability-denying Component runtime

### Task 5: Define the versioned WIT `0.1.0` packages and generate bindings

**Files:**
- Create: `crates/plugin-sdk/wit/ryuzi-plugin.wit`
- Create: `crates/plugin-sdk/wit/ryuzi-host.wit`
- Create: `crates/plugin-sdk/wit/ryuzi-settings.wit`
- Create: `crates/plugin-sdk/wit/ryuzi-storage.wit`
- Create: `crates/plugin-sdk/wit/ryuzi-http.wit`
- Create: `crates/plugin-sdk/wit/ryuzi-oauth.wit`
- Create: `crates/plugin-sdk/wit/ryuzi-gateway.wit`
- Create: `crates/plugin-sdk/wit/ryuzi-connector.wit`
- Create: `crates/plugin-sdk/wit/ryuzi-provider.wit`
- Create: `crates/plugin-sdk/wit/ryuzi-hooks.wit`
- Create: `crates/core/src/plugins/wit.rs`
- Modify: `Cargo.toml`, `crates/core/Cargo.toml`
- Test: `crates/core/src/plugins/wit.rs`

**Interfaces:**
- Produces generated Rust bindings from a single `ryuzi:plugin/plugin@0.1.0` world.
- Produces WIT records for `plugin-error`, `health`, `gateway-event`, `tool-definition`, `tool-call`, `completion-request`, `completion-chunk`, and `hook-event`.

- [ ] **Step 1: Write a compile-time binding test that imports the generated root world and constructs a `health` record.**
- [ ] **Step 2: Add `wasmtime`, `wasmtime-wasi`, and `wit-bindgen` at one pinned compatible version in `[workspace.dependencies]`; add only required dependencies to core.** Before selecting the version, check its Component Model and WASI Preview 2 support in the current official Wasmtime documentation and record the chosen version in the commit message/body.
- [ ] **Step 3: Write WIT worlds with explicit, small records and result error variants.** Do not use JSON-RPC strings or untyped JSON as the ABI. Expose shared lifecycle methods `init`, `health`, `migrate`, and `shutdown`; keep external network and secret access as imported host interfaces only.
- [ ] **Step 4: Generate bindings via `wasmtime::component::bindgen!` from `crates/plugin-sdk/wit`; compile and run the binding test.**

Run: `cargo test -p ryuzi-core plugins::wit`

Expected: PASS.

- [ ] **Step 5: Commit.**

```bash
git add Cargo.toml Cargo.lock crates/core/Cargo.toml crates/plugin-sdk/wit crates/core/src/plugins/wit.rs
git commit -m "feat(plugins): define WIT component ABI 0.1.0"
```

### Task 6: Build a generic runtime that validates but grants no capabilities by default

**Files:**
- Create: `crates/core/src/plugins/runtime.rs`
- Modify: `crates/core/src/plugins/mod.rs`
- Test: inline tests in `crates/core/src/plugins/runtime.rs`
- Create: `crates/core/tests/fixtures/component-noop/` (minimal Rust component fixture and its build script)

**Interfaces:**
- Produces `ComponentRuntime::new()`, `validate_component(&VerifiedBundle)`, `instantiate(&InstalledBundle, HostPolicy)`, and `PluginRuntimeError`.
- Produces `ResourceLimits { max_memory_bytes, fuel, timeout: Duration, max_concurrency }`.

- [ ] **Step 1: Add a test fixture component exporting only the root lifecycle world and a test proving it instantiates under the deny-all policy.** Add a second fixture importing `ryuzi:http`; assert instantiation fails when `permissions.network` is empty.
- [ ] **Step 2: Run the two tests and verify they fail before runtime implementation.**

Run: `cargo test -p ryuzi-core plugins::runtime::tests::deny_all_rejects_http_import`

Expected: FAIL.

- [ ] **Step 3: Configure Wasmtime with component model enabled, fuel consumption enabled, async support, and epoch interruption.** Build the linker from manifest-authorized imports only. Validate imports/exports against the manifest capability declarations before instantiation; reject an undeclared import or export with a stable `PluginRuntimeError` variant.
- [ ] **Step 4: Add tests for an infinite-loop fixture exhausting fuel, a sleeping fixture timing out, and a malformed component returning a non-activation error.**
- [ ] **Step 5: Run `cargo test -p ryuzi-core plugins::runtime` and `cargo clippy -p ryuzi-core --all-targets -- -D warnings`; commit.**

## Phase 3 — Host policy capabilities

### Task 7: Implement scoped settings, storage, logging, and redaction imports

**Files:**
- Create: `crates/core/src/plugins/capabilities/mod.rs`
- Create: `crates/core/src/plugins/capabilities/settings.rs`
- Create: `crates/core/src/plugins/capabilities/storage.rs`
- Create: `crates/core/src/plugins/capabilities/host.rs`
- Modify: `crates/core/src/plugins/runtime.rs`
- Modify: `crates/core/src/store.rs`
- Test: inline capability tests and store tests

**Interfaces:**
- Produces `PluginCapabilityContext { plugin_id, version, settings, store, telemetry }`.
- Produces `PluginStorage::{get, put, delete}` with keys scoped by `(plugin_id, key)`.

- [ ] **Step 1: Write tests showing plugin `github` cannot read/write `atlassian` settings or storage, secret values are unavailable through the settings value API, and log fields named `authorization`, `token`, `secret`, or declared secret settings are redacted.**
- [ ] **Step 2: Add a new storage migration with `(plugin_id, key)` primary key and bounded value size.** Include CRUD methods that use `Store::with_conn` rather than new pool code.
- [ ] **Step 3: Implement WIT imports using only the current component's `PluginCapabilityContext`; reject an undeclared setting key and enforce storage quota before write.**
- [ ] **Step 4: Run focused tests and `cargo test -p ryuzi-core plugins::capabilities`.**
- [ ] **Step 5: Commit.**

### Task 8: Implement policy-enforced HTTP and OAuth profiles

**Files:**
- Create: `crates/core/src/plugins/capabilities/http.rs`
- Create: `crates/core/src/plugins/capabilities/oauth.rs`
- Modify: `crates/core/src/plugins/oauth.rs`
- Modify: `crates/core/src/store.rs`
- Modify: `crates/core/src/api/plugins_api.rs`
- Test: inline tests plus `crates/core/tests` local HTTP-server tests

**Interfaces:**
- Produces `AllowedHttpClient::request(plugin_id, request) -> Result<SafeHttpResponse, PluginRuntimeError>`.
- Produces OAuth profile APIs `begin_pkce`, `begin_device_flow`, `poll_device_flow`, `authorized_request`, and `disconnect_profile`.

- [ ] **Step 1: Write local-server tests for allowed host success, unlisted host refusal, redirect from allowed host to unlisted host refusal, and removal/redaction of an attempted `Authorization` header supplied by a component.**
- [ ] **Step 2: Write OAuth tests proving profile IDs are isolated by plugin, a refresh token is used by host-side `authorized_request`, and component-visible response data never includes access/refresh tokens.**
- [ ] **Step 3: Extend the existing encrypted OAuth token persistence rather than creating a second secret store. Add profile identity to the key and database lookup.**
- [ ] **Step 4: Implement Device Authorization polling with interval, expiry, `authorization_pending`, `slow_down`, and cancellation behavior. Keep user code transient and out of durable telemetry/logs.**
- [ ] **Step 5: Run `cargo test -p ryuzi-core plugins::capabilities::http plugins::capabilities::oauth plugins::oauth` and commit.**

## Phase 4 — Generic capability adapters

### Task 9: Bridge component connector and hooks exports to existing session systems

**Files:**
- Create: `crates/core/src/plugins/wasm_connector.rs`
- Create: `crates/core/src/plugins/wasm_hooks.rs`
- Modify: `crates/core/src/plugins/host.rs`
- Modify: `crates/core/src/control/lifecycle.rs`
- Test: inline adapter tests and fixture components

**Interfaces:**
- Produces `WasmConnector: Connector` and `WasmHookDispatcher: ExtensionEvents` (or a replacement trait introduced at the same call site).
- Consumes WIT connector tools/invocations and typed hook events.

- [ ] **Step 1: Write fixture-component tests for tool enumeration/invocation, input/output schema validation, timeout error isolation, and `tool.before` timeout producing the documented fail-open result.**
- [ ] **Step 2: Implement adapters without branching on plugin ID. Convert WIT tool records to existing native tool/MCP records and validate every field before registering it.**
- [ ] **Step 3: Attach only enabled component plugins when a session starts; retain existing declarative connectors during this migration phase.**
- [ ] **Step 4: Run connector/hook focused tests and the affected control lifecycle tests; commit.**

### Task 10: Bridge generic component providers and long-lived gateways

**Files:**
- Create: `crates/core/src/plugins/wasm_provider.rs`
- Create: `crates/core/src/plugins/wasm_gateway.rs`
- Modify: `crates/core/src/llm_router/client.rs`
- Modify: `crates/core/src/daemon.rs`
- Modify: `crates/core/src/plugins/host.rs`
- Test: fixture-component tests, daemon tests, LLM-router tests

**Interfaces:**
- Produces generic `WasmProviderTransport` used by LLM routing and `WasmGatewaySupervisor` used by daemon startup.
- Consumes an installed bundle's manifest lifecycle/capabilities and active version; no plugin ID enters either API.

- [ ] **Step 1: Write a provider fixture test that returns a static model and a streaming two-chunk completion; assert generic LLM routing preserves chunk order and converts a trap to a route-scoped error.**
- [ ] **Step 2: Write a gateway fixture test that emits a typed inbound message after `start`, accepts an outbound message, exposes health, and is restarted with capped backoff after a trap.**
- [ ] **Step 3: Implement one supervisor task per enabled long-lived bundle, with graceful `stop`, bounded restart window, status snapshots, and daemon-shutdown ownership.**
- [ ] **Step 4: Run targeted daemon/router/plugin tests and commit.**

## Phase 5 — Bootstrap providers and catalog/UI integration

### Task 11: Extend signed catalog/install APIs and bootstrap Mimo/OpenCode

**Files:**
- Modify: `crates/core/src/plugins/remote_catalog.rs`
- Modify: `crates/core/src/api/plugins_api.rs`
- Modify: `crates/core/src/api/types.rs`
- Modify: `crates/core/src/daemon.rs`
- Create: `plugins/mimo/`, `plugins/opencode/`
- Create: `scripts/plugins/build-first-party.ts`
- Test: core API/daemon tests; plugin component unit tests

**Interfaces:**
- Produces RPCs `plugin_release_detail`, `install_component_plugin`, `rollback_component_plugin`, and profile-aware OAuth calls.
- Produces bootstrap retry state returned to Cockpit.

- [ ] **Step 1: Write daemon tests with a fake signed catalog HTTP client proving first run attempts Mimo/OpenCode, records each successful release independently, and reports retryable bootstrap status when both downloads fail without failing `build_daemon`.**
- [ ] **Step 2: Implement catalog release resolution and download using the existing signed-feed mechanism as a base; do not treat a catalog manifest as executable code.**
- [ ] **Step 3: Implement the Mimo and OpenCode components and manifests as first-party bundles using only the provider WIT world and exact required API domains. Port their existing free-tier request behavior behind authorized host HTTP.**
- [ ] **Step 4: Add scripts that reproducibly compile each component, emit release JSON, SHA-256, and detached signature. Never commit private signing keys.**
- [ ] **Step 5: Run core tests, component tests, and `bun run --cwd apps/cockpit build` after bindings regenerate; commit.**

### Task 12: Update Cockpit bundle management UX

**Files:**
- Modify: `apps/cockpit/src/views/PluginsView.tsx`
- Modify: `apps/cockpit/src/views/PluginDetailView.tsx`
- Modify: Cockpit hooks/store files identified by `rg "usePlugins|plugin_detail|begin_plugin_install" apps/cockpit/src`
- Regenerate: `apps/cockpit/src/bindings.ts`
- Test: matching Cockpit unit tests and E2E tests

**Interfaces:**
- Consumes the Phase 5 generated RPC DTOs.
- Renders release version, publisher verification, domains, OAuth scopes/profiles, lifecycle, install permission confirmation, update, pin, rollback, health, and redacted doctor output.

- [ ] **Step 1: Write tests for permission summary rendering, disabled install confirmation until accepted, one active version display, pin/rollback action dispatch, and retryable bootstrap banner.**
- [ ] **Step 2: Implement using `@ryuzi/ui` primitives only; do not add raw button/input/select elements.**
- [ ] **Step 3: Regenerate bindings via the repository's existing binding-generation command discovered from `package.json`/Tauri tooling; do not edit `bindings.ts` by hand.**
- [ ] **Step 4: Run targeted Bun tests and `bun run --cwd apps/cockpit build`; commit.**

## Phase 6 — First-party integration pilots

### Task 13: Deliver GitHub connector `0.1.x`

**Files:**
- Create: `plugins/github/`
- Modify: first-party catalog publication data under the path introduced in Task 11
- Test: `plugins/github` component tests and core install/connector/OAuth approval tests

**Interfaces:**
- Exports GitHub connector tools for auth status, profile, repository metadata, issues, pull requests, and REST/GraphQL API calls.
- Uses OAuth profile `github` with Device Flow default, PKCE optional, and manual token fallback.

- [ ] **Step 1: Write component tests against a mock GitHub HTTP host for Device Flow initiation, profile/status, repo list/view, issue list/create/comment, PR list/create/review/merge, and REST/GraphQL calls.**
- [ ] **Step 2: Require explicit approval metadata for every mutating tool, including merge/delete, release publication, secret write, workflow dispatch, and organization mutations; add tests that an unapproved request is not sent.**
- [ ] **Step 3: Implement `0.1.0` only. Keep later release domains (releases/workflows/gists/etc.) out of this implementation and track them as subsequent plugin releases, not host work.**
- [ ] **Step 4: Sign/install the test release through the generic pipeline and run end-to-end connector/OAuth tests; commit.**

### Task 14: Migrate Discord to the generic gateway and remove native paths

**Files:**
- Create: `plugins/discord/`
- Delete: `crates/core/src/plugins/builtin.rs` after any non-Discord responsibility is relocated
- Modify: `crates/core/src/daemon.rs`, `crates/core/src/plugins/mod.rs`, `crates/core/src/plugins/host.rs`, `crates/core/src/settings/catalog.rs`, relevant runner tests, and docs
- Delete or retire: `crates/core/src/gateway/discord.rs` only after component behavior proves replacement

**Interfaces:**
- Discord component exports only the generic gateway WIT world and declares token settings plus Discord domains.
- Daemon consumes `WasmGatewaySupervisor`, not a Discord factory map or `enabled_gateways` CSV.

- [ ] **Step 1: Write migration tests proving an installed/enabled generic gateway starts, disabled gateways do not start, and no test configuration names Discord outside the Discord bundle fixture.**
- [ ] **Step 2: Port protocol/event translation, reconnect, health, idempotency, and graceful shutdown into `plugins/discord`; keep bot tokens in host secret storage.**
- [ ] **Step 3: Remove Discord default config, static config fields, `builtin::discord_plugin`, native factory registration, `extra_gateway_factories` dependency for production Discord, and every `enabled_gateways = discord` assumption.**
- [ ] **Step 4: Run `rg -n "discord_plugin|factory_entries|enabled_gateways = discord|plugin.id == \\"discord\\"" crates apps` and require no runtime hit outside migration tests/docs; run all core and runner tests; commit.**

### Task 15: Deliver Atlassian and Bitbucket as separate connector bundles

**Files:**
- Create: `plugins/atlassian/`
- Create: `plugins/bitbucket/`
- Modify: catalog publication data and Cockpit plugin metadata tests
- Test: each plugin's component/mock-HTTP tests and generic OAuth profile integration tests

**Interfaces:**
- `atlassian` exports Jira and Confluence tools using one `atlassian-cloud` 3LO profile.
- `bitbucket` exports Bitbucket Cloud tools using a distinct `bitbucket-cloud` OAuth consumer/profile.

- [ ] **Step 1: Write tests proving Jira and Confluence requests share only the Atlassian Cloud profile, while every Bitbucket request fails without the separate Bitbucket profile.**
- [ ] **Step 2: Implement typed read and mutation tools per product with host approval metadata for mutations; constrain domains to `api.atlassian.com`, allowed tenant suffixes, and `api.bitbucket.org` respectively.**
- [ ] **Step 3: Verify Cockpit renders two independent install/detail/connection experiences and never claims a single token serves all products.**
- [ ] **Step 4: Run component, core, Cockpit tests, then commit.**

## Phase 7 — Completion migrations and release hardening

### Task 16: Migrate remaining providers and retire static provider discovery

**Files:**
- Create: one `plugins/<provider-id>/` project per supported provider
- Modify/Delete: `crates/core/src/plugins/providers.rs`, static provider registration in `crates/core/src/llm_router/registry.rs`, provider-specific client paths once ported
- Modify: docs/development/plugins.md
- Test: provider conformance suite plus LLM-router regressions

**Interfaces:**
- Each provider conforms to the generic provider WIT world and passes the same model/stream/error conformance suite.

- [ ] **Step 1: Build a conformance suite that runs every provider component against mocked allowed HTTP endpoints and verifies model listing, non-stream completion, stream ordering, auth absence, HTTP error mapping, and timeout mapping.**
- [ ] **Step 2: Port one provider at a time, adding it to signed catalog publication only after it passes the suite. Do not remove its native path until its component release passes all router regressions.**
- [ ] **Step 3: When all providers pass, remove `install_providers`, static runtime provider discovery, and provider-ID-specific router transport selection.**
- [ ] **Step 4: Run `cargo test -p ryuzi-core -p ryuzi-runner`, `cargo clippy -p ryuzi-core -p ryuzi-runner --all-targets -- -D warnings`, and commit.**

### Task 17: Finalize revocation, doctor, documentation, and release checks

**Files:**
- Modify: `crates/core/src/plugins/doctor.rs`, `crates/core/src/plugins/remote_catalog.rs`, `crates/core/src/api/plugins_api.rs`, `docs/development/plugins.md`
- Modify: Cockpit plugin views/tests as needed
- Modify: release scripts/workflows only after inspecting `.github/workflows`, `scripts/npm`, and packaging docs
- Test: core/runner/Cockpit suites and installer smoke tests

**Interfaces:**
- Doctor reports signature, hash, ABI compatibility, active/revoked state, policy violations, OAuth profile health, and long-lived restart exhaustion without secrets.

- [ ] **Step 1: Write tests showing a signed revocation disables an active gateway at a safe boundary, blocks enablement and rollback, preserves a redacted reason, and leaves unrelated releases usable.**
- [ ] **Step 2: Update documentation to replace embedded catalog/native Discord claims with component bundle, permission, OAuth-profile, and recovery behavior.**
- [ ] **Step 3: Run the full required verification matrix:**

```bash
cargo fmt --check
cargo clippy -p ryuzi-core -p ryuzi-runner --all-targets -- -D warnings
cargo test -p ryuzi-core -p ryuzi-runner
bun test
bun run typecheck
bun run --cwd apps/cockpit build
cargo build -p ryuzi-runner
./target/debug/ryuzi --version
./target/debug/ryuzi --help
```

Expected: every command exits 0. If a platform-specific command cannot run in the worktree, record the exact command, failure reason, and remaining verification gap.

- [ ] **Step 4: Inspect `git diff --check`, confirm no private signing key or generated artifact accidentally entered the worktree, and commit the final hardening change.**

## Plan self-review

- **Spec coverage:** Tasks 1–4 cover versioned signed bundles, catalog/ledger, activation, pin/rollback prerequisites, and bootstrap prerequisites. Tasks 5–10 cover all WIT worlds, generic runtime, limits, hybrid lifetime, storage, network, OAuth, connector, hooks, providers, and gateway adapters. Tasks 11–15 cover bootstrap Mimo/OpenCode, Cockpit/runner behavior, GitHub, Discord, Atlassian, and Bitbucket. Tasks 16–17 cover remaining providers, revocation, doctor, documentation, and release verification.
- **Intentional sequencing:** full GitHub CLI parity is explicitly not promised by `github@0.1.0`; later GitHub domains remain independently released plugin versions. Third-party publisher onboarding and arbitrary unsigned local plugins remain outside this plan's scope.
- **Consistency check:** all tasks use the same SDK `PluginBundleManifest`/`PluginRelease`, core `VerifiedBundle`/`InstalledBundle`, and generic component runtime. No task introduces a plugin-ID-specific host branch.
- **Placeholder scan:** no TODO/TBD/future implementation markers are used as executable steps. Later plugin release scope is explicitly excluded where it is a product roadmap rather than a host-platform requirement.
