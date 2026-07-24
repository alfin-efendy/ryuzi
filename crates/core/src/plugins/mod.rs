//! Plugin binding layer.
//!
//! A [`ryuzi_plugin_sdk::PluginManifest`] is purely declarative — identity,
//! metadata, settings schema, and so on. This module binds a manifest to the
//! behavioral capabilities it actually provides at runtime
//! ([`host::CorePlugin`]), tracks every installed plugin
//! ([`host::PluginHost`]), and is the new home of [`host::Registries`] (moved
//! here from the deleted `integration` module, which this layer replaces
//! entirely — see `host`'s module doc for how `Registries::add_plugin`
//! supersedes the old `Integration` trait).
//!
//! The `native` first-party plugin lives beside its harness code in
//! `harness::native`. Gateways ship as signed WASM component bundles (the
//! migrated Discord component and any future one), discovered off-disk and
//! driven through `plugins::wasm_gateway_bridge::WasmGateway` — there is no
//! native (in-process) gateway plugin.
//!
//! [`providers`] generates manifest-only plugins from the static provider
//! catalog (`llm_router::registry::CATALOG`) rather than hand-authoring one
//! manifest per entry. [`install_builtins`] adds them plus the embedded
//! catalog in one call.

pub mod bundle;
pub mod capabilities;
pub mod catalog_feed_key;
pub mod component_catalog;
pub mod declarative;
pub mod doctor;
pub mod extension;
pub mod first_party_key;
pub mod host;
pub mod oauth;
pub mod providers;
pub mod remote_catalog;
pub mod runtime;
pub mod wasm_connector;
pub mod wasm_gateway;
pub mod wasm_gateway_bridge;
pub mod wasm_hooks;
pub mod wasm_provider;
pub mod wit;

/// Reusable provider-conformance harness (plan Task 16, Step 1): runs a
/// provider component through the generic [`wasm_provider`] seam against a
/// mocked, allowlisted HTTP upstream and verifies model listing,
/// completion/order, auth absence, HTTP error mapping, and timeout mapping.
/// Test-only, and proven against the synthetic `component-provider-http`
/// fixture; later slices reuse it per real provider component.
#[cfg(test)]
mod wasm_provider_conformance;

/// End-to-end proof that the REAL first-party GitHub connector component
/// (`plugins/github`) signs, installs, enumerates its tools, and drives its
/// host-managed OAuth through the GENERIC pipeline (Task 13b, Step 4). Kept in
/// its own module so the pilot's evidence lives in one place; it exercises only
/// the generic seams (no github-specific host branch).
#[cfg(test)]
mod github_e2e;

/// De-risking proof that the REAL first-party Discord gateway component
/// (`plugins/discord`) signs, installs, and INSTANTIATES through the host
/// [`runtime::ComponentRuntime`] with the `ryuzi:websocket` capability linked —
/// the gateway analog of `github_e2e` (Task 10b). Catches any import/world
/// mismatch before the expensive real-bot manual smoke; exercises only the
/// generic seams shipped by Phases 1-5 (no discord-specific host branch).
#[cfg(test)]
mod discord_e2e;

/// Task 15c — the OAuth-profile ISOLATION proof for the REAL first-party
/// Atlassian (`plugins/atlassian`) and Bitbucket (`plugins/bitbucket`)
/// connector components: both sign, install, and load through the generic
/// pipeline, and a token seeded ONLY for `(atlassian, atlassian-cloud)` lets
/// both Jira- and Confluence-style requests through that ONE profile while
/// every Bitbucket request is denied absent its own separate
/// `(bitbucket, bitbucket-cloud)` token — proving the two connectors never
/// share a token, keyed purely by the generic `(plugin_id, profile_id)` store
/// + `ProfileOauth::ensure_declared_profile`, with no plugin-id host branch.
#[cfg(test)]
mod atlassian_bitbucket_e2e;

use crate::settings::{csv, SettingsStore};
use crate::store::Store;

pub use doctor::{plugin_doctor, DoctorFinding};
pub use extension::{
    ExtensionCtx, ExtensionEvents, ExtensionFactory, ExtensionHost, ExtensionProc,
    ExtensionSnapshot, ExtensionSpec, ExtensionStatus,
};
pub use host::{plugin_field, plugin_fields_all, CorePlugin, PluginHost, PluginSource, Registries};

/// Build every `tests/fixtures/*` component EXACTLY ONCE per test process.
///
/// The fixture-backed tests run concurrently (multi-thread tokio), and
/// `build-components.sh`'s `materialize_deps` rewrites each fixture's
/// `wit/deps/` non-atomically (`rm -rf` then repopulate) — so two concurrent
/// invocations corrupt each other and `cargo build` fails. Routing every
/// fixture test (in `runtime`, `wasm_connector`, `wasm_hooks`) through this
/// shared `OnceLock` serializes the build to a single run the whole binary
/// waits on, then reuses the artifacts.
#[cfg(test)]
pub(crate) fn build_fixture_components_once() {
    use std::sync::OnceLock;
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let script = root
            .join("tests")
            .join("fixtures")
            .join("build-components.sh");
        let status = std::process::Command::new("sh")
            .arg(script)
            .status()
            .expect("fixture build script should start");
        assert!(status.success(), "fixture build failed: {status}");
    });
}

/// Build the first-party GitHub connector component (`plugins/github`) to
/// wasm32-wasip2 EXACTLY ONCE per test process.
///
/// `plugins/github` is a STANDALONE workspace crate (like `plugins/mimo`), not
/// a `tests/fixtures/*` fixture, so [`build_fixture_components_once`] does not
/// build it. This sibling helper runs `tests/fixtures/build-github-component.sh`,
/// which materializes `plugins/github/wit/deps` from the SDK and builds the
/// component the same way `scripts/plugins/build-first-party.ts` does. The
/// script touches only `plugins/github/wit/deps` (gitignored), so it never
/// races the fixture builder's rewrites of the `tests/fixtures/*` deps. Cached
/// via its own `OnceLock` so the (real) github sign/install/connector e2e tests
/// share a single build.
#[cfg(test)]
pub(crate) fn build_github_component_once() {
    use std::sync::OnceLock;
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let script = root
            .join("tests")
            .join("fixtures")
            .join("build-github-component.sh");
        let status = std::process::Command::new("sh")
            .arg(script)
            .status()
            .expect("github component build script should start");
        assert!(status.success(), "github component build failed: {status}");
    });
}

/// Build the first-party Discord gateway component (`plugins/discord`) to
/// wasm32-wasip2 EXACTLY ONCE per test process.
///
/// `plugins/discord` is a STANDALONE workspace crate (like `plugins/github`),
/// not a `tests/fixtures/*` fixture, so [`build_fixture_components_once`] does
/// not build it. This sibling helper runs
/// `tests/fixtures/build-discord-component.sh`, which materializes
/// `plugins/discord/wit/deps` from the SDK and builds the component the same
/// way `scripts/plugins/build-first-party.ts` does. The script touches only
/// `plugins/discord/wit/deps` (gitignored), so it never races the fixture
/// builder's or the github builder's rewrites of their own deps. Cached via
/// its own `OnceLock` so the (real) discord instantiation e2e tests share a
/// single build.
#[cfg(test)]
pub(crate) fn build_discord_component_once() {
    use std::sync::OnceLock;
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let script = root
            .join("tests")
            .join("fixtures")
            .join("build-discord-component.sh");
        let status = std::process::Command::new("sh")
            .arg(script)
            .status()
            .expect("discord component build script should start");
        assert!(status.success(), "discord component build failed: {status}");
    });
}

/// Build the first-party Atlassian connector component (`plugins/atlassian`)
/// to wasm32-wasip2 EXACTLY ONCE per test process.
///
/// `plugins/atlassian` is a STANDALONE workspace crate (like `plugins/github`),
/// not a `tests/fixtures/*` fixture, so [`build_fixture_components_once`] does
/// not build it. This sibling helper runs
/// `tests/fixtures/build-atlassian-component.sh`, which materializes
/// `plugins/atlassian/wit/deps` from the SDK and builds the component the same
/// way `scripts/plugins/build-first-party.ts` does. The script touches only
/// `plugins/atlassian/wit/deps` (gitignored), so it never races the other
/// builders' rewrites of their own deps. Cached via its own `OnceLock` so the
/// (real) atlassian/bitbucket OAuth-isolation e2e tests share a single build.
#[cfg(test)]
pub(crate) fn build_atlassian_component_once() {
    use std::sync::OnceLock;
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let script = root
            .join("tests")
            .join("fixtures")
            .join("build-atlassian-component.sh");
        let status = std::process::Command::new("sh")
            .arg(script)
            .status()
            .expect("atlassian component build script should start");
        assert!(
            status.success(),
            "atlassian component build failed: {status}"
        );
    });
}

/// Build the first-party Bitbucket connector component (`plugins/bitbucket`)
/// to wasm32-wasip2 EXACTLY ONCE per test process. Sibling of
/// [`build_atlassian_component_once`] — see that function's doc.
#[cfg(test)]
pub(crate) fn build_bitbucket_component_once() {
    use std::sync::OnceLock;
    static BUILT: OnceLock<()> = OnceLock::new();
    BUILT.get_or_init(|| {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let script = root
            .join("tests")
            .join("fixtures")
            .join("build-bitbucket-component.sh");
        let status = std::process::Command::new("sh")
            .arg(script)
            .status()
            .expect("bitbucket component build script should start");
        assert!(
            status.success(),
            "bitbucket component build failed: {status}"
        );
    });
}

/// Build the first-party LLM PROVIDER component in `plugins/<plugin_dir>` to
/// wasm32-wasip2 EXACTLY ONCE per test process, per directory. Sibling of
/// [`build_atlassian_component_once`] — see that function's doc — but
/// parameterized, because the provider migration ships one such bundle per
/// OpenAI-format provider and they are otherwise built identically.
///
/// Consumed by the provider conformance battery
/// (`crate::plugins::wasm_provider_conformance`), which drives each real
/// component against a loopback mock upstream. The builds are serialized behind
/// one mutex so concurrent conformance tests do not race cargo, and each
/// directory is built at most once however many tests ask for it.
#[cfg(test)]
pub(crate) fn build_provider_component_once(plugin_dir: &str) {
    use std::collections::HashSet;
    use std::sync::{Mutex, OnceLock};
    static BUILT: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();
    let mut built = BUILT
        .get_or_init(|| Mutex::new(HashSet::new()))
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if built.contains(plugin_dir) {
        return;
    }
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    let script = root
        .join("tests")
        .join("fixtures")
        .join("build-provider-component.sh");
    let status = std::process::Command::new("sh")
        .arg(script)
        .arg(plugin_dir)
        .status()
        .expect("provider component build script should start");
    assert!(
        status.success(),
        "{plugin_dir} component build failed: {status}"
    );
    // Recorded only AFTER the build succeeded. Recording it up front would
    // make a FAILED build look done, so the next test in this process would
    // skip the build and fail on a missing artifact instead of on the real
    // compile error. The `assert!` above unwinds before this line, leaving the
    // directory unrecorded so the next caller retries and sees the true error.
    built.insert(plugin_dir.to_string());
}

/// Add every generated manifest-only builtin — every model provider
/// ([`providers::provider_plugins`]) — to `regs`. Factored out of
/// [`install_builtins`] so the daemon composition root can add providers
/// first and the component catalog after, matching this function's own
/// ordering guarantee.
pub fn install_providers(regs: &mut Registries) {
    for plugin in providers::provider_plugins() {
        regs.add_plugin(plugin);
    }
}

/// Add every generated manifest-only builtin — every model provider
/// ([`providers::provider_plugins`]) plus the first-party component catalog
/// ([`component_catalog::component_catalog_plugins`]) — to `regs`.
///
/// This deliberately does NOT add `native`: it carries host-injected config
/// that only the composition root can supply, so hosts add it first and call
/// `install_builtins` afterward.
///
/// The component catalog is added last: `Registries::add_plugin` keeps the
/// first registration for a colliding id, so providers (added above) always
/// win over a same-id component bundle, and both lose to `native` (added by
/// the composition root before calling this function).
///
/// `daemon::build_daemon` does not call this — it inlines the same two steps
/// in the same order so it can interleave skill-pack loading — but the
/// resulting registration set is identical.
pub fn install_builtins(regs: &mut Registries) {
    // Order is load-bearing: `Registries::add_plugin` keeps the FIRST
    // registration for an id. Providers must precede the component catalog so
    // a colliding provider component never displaces its builtin's richer
    // manifest (seed models, auth spec) — see `component_catalog`'s module doc.
    install_providers(regs);
    for plugin in component_catalog::component_catalog_plugins() {
        regs.add_plugin(plugin);
    }
}

/// Populate the process-wide `PLUGIN_FIELDS` registry (see `host`'s module
/// doc) with every built-in plugin's settings keys, without any of the
/// side-effectful or networked work a full composition root does.
///
/// Callers that only need `validate_setting`/`is_secret` to recognize
/// `plugin.*` keys (e.g. `ryuzi config get/set/list`) should call this
/// instead of building a real `Registries` — in particular, this
/// deliberately avoids side-effectful operations like spawning processes
/// or touching the network, keeping output clean without noisy diagnostic notes.
///
/// The built `Registries` value is dropped at the end of this function:
/// registration into `PLUGIN_FIELDS` is a side effect of
/// `Registries::add_plugin` (`host::register_plugin_fields`), not something
/// read back from the `Registries` itself.
pub fn register_builtin_plugin_fields() {
    let mut regs = Registries::new();
    regs.add_plugin(crate::harness::native::native_plugin());
    install_builtins(&mut regs);
    load_skill_pack_plugins(&mut regs);
}

/// Discover and register installed skill-pack plugins from
/// `~/.config/ryuzi/plugins/*/ryuzi-plugin.toml`. Call after
/// [`install_builtins`] so a skill-pack manifest can never shadow a
/// built-in (`Registries::add_plugin` keeps the first registration for a
/// given id — see `host`'s module doc).
///
/// Only directories the skills installer produced are accepted: the
/// directory must carry a `.ryuzi-skill.json` provenance stamp
/// (`skills_install::install_plugin_pack` writes it), or — legacy packs
/// installed before the stamp existed — the directory's own name must
/// equal the manifest's plugin id *and* the skills root must hold
/// materialized provenance naming that same id, in which case the stamp
/// is healed into the plugin directory one time. The dir-name check
/// blocks a hand-authored directory from spoofing another installed
/// pack's id to ride its materialized provenance into a heal. Hand-authored
/// manifests match neither and are skipped with a `tracing::warn!`.
///
/// A missing config directory is not an error (most installs have none).
/// A plugin directory that fails to parse or fails manifest validation is
/// logged via `tracing::warn!` and skipped — never panics, and never
/// stops the rest of the scan.
pub fn load_skill_pack_plugins(regs: &mut Registries) {
    let Some(home) = dirs::home_dir() else {
        tracing::warn!("could not resolve home directory — skipping skill-pack plugin discovery");
        return;
    };
    let config = home.join(".config/ryuzi");
    load_skill_pack_plugins_from(regs, &config.join("plugins"), &config.join("skills"));
}

/// The scan behind [`load_skill_pack_plugins`], factored out so tests can
/// pass tempdirs instead of the real config directories.
pub(crate) fn load_skill_pack_plugins_from(
    regs: &mut Registries,
    plugins_root: &std::path::Path,
    skills_root: &std::path::Path,
) {
    let Ok(entries) = std::fs::read_dir(plugins_root) else {
        return; // no skill-pack plugin directory — nothing to do
    };
    for entry in entries.filter_map(Result::ok) {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let manifest_path = dir.join("ryuzi-plugin.toml");
        let text = match std::fs::read_to_string(&manifest_path) {
            Ok(text) => text,
            Err(_) => continue, // no manifest in this directory — not a plugin
        };
        let manifest = match ryuzi_plugin_sdk::PluginManifest::from_toml(&text) {
            Ok(manifest) => manifest,
            Err(e) => {
                tracing::warn!(
                    "skipping skill-pack plugin at {}: invalid manifest: {e}",
                    manifest_path.display()
                );
                continue;
            }
        };
        // Skill-pack provenance gate: accept the installer's stamp, or
        // heal a legacy install from the skills root's materialized
        // provenance; skip hand-authored manifests (neither).
        let stamped = dir.join(crate::skills_install::PROVENANCE_FILE).is_file()
            || crate::skills_install::stamp_legacy_skill_pack_provenance(
                skills_root,
                &dir,
                &manifest.id,
            );
        if !stamped {
            tracing::warn!(
                "skipping {}: not an installed skill pack (no .ryuzi-skill.json stamp and no \
                 materialized skill provenance for `{}` in the skills root) — hand-authored \
                 plugin manifests are no longer loaded",
                manifest_path.display(),
                manifest.id
            );
            continue;
        }
        match declarative::declarative_plugin(manifest, PluginSource::SkillPack(dir.clone())) {
            Ok(plugin) => regs.add_plugin(plugin),
            Err(e) => {
                tracing::warn!(
                    "skipping skill-pack plugin at {}: {e}",
                    manifest_path.display()
                );
            }
        }
    }
}

/// Toggle `id`'s enablement — the single source of truth behind Cockpit's
/// `set_plugin_enabled` command (the only toggle surface; there is no CLI
/// equivalent), so the write side can never drift from
/// [`PluginHost::is_enabled`]'s read side:
/// - unknown id → an error (`"unknown plugin: {id}"`)
/// - harness-capable → an error (the native runtime is always enabled)
/// - gateway-capable → add/remove `id` in the `enabled_gateways` CSV setting
/// - experimental (docs-only, no capability) → an error, since
///   `is_enabled` always reports it disabled regardless of any
///   `plugin.<id>.enabled` write (see that method's doc) — toggling would
///   silently no-op
/// - no harness/gateway/connector capability (manifest-only, e.g. a
///   provider metadata entry) → an error, since `is_enabled`
///   always reports it enabled regardless of any `plugin.<id>.enabled`
///   write — toggling would silently no-op
/// - connector-only → set `plugin.<id>.enabled` to `"true"`/`"false"`
pub async fn toggle_enabled(
    host: &PluginHost,
    settings: &SettingsStore,
    id: &str,
    enable: bool,
) -> anyhow::Result<()> {
    let Some(plugin) = host.get(id) else {
        anyhow::bail!("unknown plugin: {id}");
    };
    if enable {
        let (blocked, reason) = is_blocked(&settings.store(), id).await;
        if blocked {
            anyhow::bail!(
                "blocked by catalog: {}",
                reason.unwrap_or_else(|| "revoked".into())
            );
        }
    }
    if plugin.harness.is_some() {
        anyhow::bail!("{id} is always enabled");
    }
    if plugin.gateway.is_some() {
        return toggle_csv(settings, "enabled_gateways", id, enable).await;
    }
    if plugin.manifest.experimental {
        anyhow::bail!("{id} is experimental — nothing to enable");
    }
    if plugin.connector.is_none() {
        anyhow::bail!("{id} is always available");
    }
    settings
        .set(
            &format!("plugin.{id}.enabled"),
            if enable { "true" } else { "false" },
        )
        .await
}

/// Add (or remove) `id` in a CSV settings value, preserving the existing
/// entries' order and never introducing a duplicate.
async fn toggle_csv(
    settings: &SettingsStore,
    key: &str,
    id: &str,
    enable: bool,
) -> anyhow::Result<()> {
    let mut values = csv(settings.get(key).await?.as_deref());
    if enable {
        if !values.iter().any(|v| v == id) {
            values.push(id.to_string());
        }
    } else {
        values.retain(|v| v != id);
    }
    settings.set(key, &values.join(",")).await
}

/// Whether the remote catalog's signed feed has blocked `id`, per the cached
/// `plugin_catalog_cache` rows Task 3's fetch pipeline writes
/// ([`remote_catalog::fetch_and_cache`]). A store read failure is treated as
/// "not blocked" — a transient DB hiccup must never itself refuse an enable
/// or manufacture a doctor finding.
pub async fn is_blocked(store: &Store, id: &str) -> (bool, Option<String>) {
    match store.list_remote_catalog().await {
        Ok(rows) => rows
            .into_iter()
            .find(|r| r.id == id && r.blocked)
            .map(|r| (true, r.blocked_reason))
            .unwrap_or((false, None)),
        Err(_) => (false, None),
    }
}

/// Live-disable every currently-enabled plugin whose id the feed blocked.
/// Future enables are refused separately by [`toggle_enabled`]'s
/// [`is_blocked`] short-circuit; this sweep only needs to handle plugins that
/// were already enabled *before* the block took effect. No restart is
/// needed — the session-attach loop re-reads [`PluginHost::is_enabled`] per
/// session, so flipping the setting here takes effect on the next attach.
///
/// Best-effort per id: a single plugin's settings write failing is logged
/// and does not abort the rest of the sweep.
///
/// Scope note: the `plugin.<id>.enabled=false` key this writes is the
/// *connector*-plugin enable flag. It is a deliberate no-op for gateway ids
/// (which are toggled via the `enabled_gateways` CSV, not per-id settings) and
/// for harness- or manifest-only ids. That is correct for the real domain
/// here — remote-catalog entries are always connector plugins, so a blocked id
/// always maps to this key — but do not repurpose this sweep for
/// gateway/harness blocks without also handling their distinct enable
/// mechanisms.
pub async fn apply_blocked_denylist(
    store: &Store,
    settings: &SettingsStore,
    host: &PluginHost,
) -> anyhow::Result<Vec<String>> {
    let blocked: Vec<String> = store
        .list_remote_catalog()
        .await?
        .into_iter()
        .filter(|r| r.blocked)
        .map(|r| r.id)
        .collect();
    for id in &blocked {
        if host.get(id).is_some() && host.is_enabled(settings, id).await.unwrap_or(false) {
            match settings.set(&format!("plugin.{id}.enabled"), "false").await {
                Ok(()) => tracing::warn!("catalog: auto-disabled blocked plugin {id}"),
                Err(e) => {
                    tracing::warn!("catalog: failed to auto-disable blocked plugin {id}: {e}")
                }
            }
        }
    }
    // The blocked ids are returned so a caller holding the daemon's live
    // gateways can ALSO stop any running gateway among them at a safe boundary
    // (`stop_revoked_gateways`) — the enable-flag flip above only takes effect
    // on the next attach/restart.
    Ok(blocked)
}

/// Gracefully stop every currently-running gateway whose plugin id was
/// revoked/blocked, returning the ids actually stopped.
///
/// This closes the mid-session revocation gap: [`apply_blocked_denylist`] and
/// the component-release revoke path flip the *enablement setting* (which only
/// takes effect on the next attach/restart), but a long-lived WASM **gateway**
/// keeps its supervisor running until then. Given the daemon's live gateways
/// and the just-revoked id set, this disables the running ones at a SAFE
/// boundary by calling each matching gateway's EXISTING graceful
/// [`Gateway::stop`] — which, for a `WasmGateway`, stops the supervisor's
/// component and aborts its task; no new teardown path is invented.
///
/// GENERIC by construction: it matches purely on [`Gateway::id`], never on a
/// specific plugin id, so the migrated Discord gateway (or any future
/// long-lived gateway component) is disabled the same way. A revoked id with no
/// currently-running gateway — a connector/provider, or a gateway not presently
/// supervised — matches nothing and is a clean no-op, never an error.
///
/// Resilient: a `stop()` that errors (e.g. it races a trap/restart the
/// supervisor already guards) is logged and swallowed, never propagated — one
/// gateway's stop can neither abort the sweep nor crash the daemon. The stopped
/// id is still recorded either way, because `Gateway::stop` tears the
/// supervisor down (aborts its task) even on the component-stop error path.
pub async fn stop_revoked_gateways(
    gateways: &[std::sync::Arc<dyn crate::gateway::Gateway>],
    revoked_ids: &std::collections::HashSet<String>,
) -> Vec<String> {
    let mut stopped = Vec::new();
    for gateway in gateways {
        if !revoked_ids.contains(gateway.id()) {
            continue;
        }
        if let Err(error) = gateway.stop().await {
            tracing::warn!(
                gateway = %gateway.id(),
                "revocation: graceful stop of a revoked gateway errored (swallowed): {error}"
            );
        }
        stopped.push(gateway.id().to_string());
    }
    stopped
}

#[cfg(test)]
mod toggle_enabled_tests {
    use super::*;
    use crate::connector::{Connector, ConnectorCtx};
    use crate::domain::{ApprovalDecision, ApprovalRequest, McpServerSpec, Surface};
    use crate::gateway::{Gateway, GatewayFactory, MessageRef};
    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx};
    use crate::store::Store;
    use async_trait::async_trait;
    use ryuzi_plugin_sdk::PluginManifest;
    use std::sync::Arc;

    // ---- minimal fakes, self-contained to this test module (mirrors host.rs's tests) ----

    struct FakeHarness;
    #[async_trait]
    impl Harness for FakeHarness {
        async fn start_session(&self, _ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            anyhow::bail!("not needed in this test")
        }
    }
    struct FakeHarnessFactory;
    impl HarnessFactory for FakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(FakeHarness))
        }
    }

    struct FakeGateway;
    #[async_trait]
    impl Gateway for FakeGateway {
        fn id(&self) -> &str {
            "fake"
        }
        async fn start(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn create_workspace(&self, name: &str) -> anyhow::Result<String> {
            Ok(format!("ws-{name}"))
        }
        async fn create_conversation(
            &self,
            _workspace_id: &str,
            _title: &str,
        ) -> anyhow::Result<String> {
            Ok("conv".to_string())
        }
        async fn post_status(&self, surface: &Surface, _text: &str) -> anyhow::Result<MessageRef> {
            Ok(MessageRef {
                surface: surface.clone(),
                message_id: "m1".to_string(),
            })
        }
        async fn edit_status(&self, _msg: &MessageRef, _text: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_result(&self, _surface: &Surface, _chunks: &[String]) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_error(&self, _surface: &Surface, _message: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _s: &Surface,
            _r: &ApprovalRequest,
        ) -> anyhow::Result<ApprovalDecision> {
            Ok(ApprovalDecision::Cancel)
        }
    }
    struct FakeGatewayFactory;
    impl GatewayFactory for FakeGatewayFactory {
        fn create(&self, _c: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>> {
            Ok(Arc::new(FakeGateway))
        }
    }

    struct FakeConnector;
    #[async_trait]
    impl Connector for FakeConnector {
        async fn mcp_servers(&self, _ctx: &ConnectorCtx) -> anyhow::Result<Vec<McpServerSpec>> {
            Ok(vec![])
        }
    }

    /// A `Gateway` that records how many times `stop()` was called, so a
    /// revocation test can assert the live-stop mechanism reached exactly the
    /// right gateway (and left unrelated ones untouched). `fail_stop` makes
    /// `stop()` return an error so the sweep's resilience can be exercised.
    struct RecordingGateway {
        id: String,
        stops: Arc<std::sync::atomic::AtomicUsize>,
        fail_stop: bool,
    }
    impl RecordingGateway {
        fn new(id: &str) -> Self {
            Self {
                id: id.to_string(),
                stops: Arc::new(std::sync::atomic::AtomicUsize::new(0)),
                fail_stop: false,
            }
        }
        fn failing(id: &str) -> Self {
            Self {
                fail_stop: true,
                ..Self::new(id)
            }
        }
        fn stops(&self) -> usize {
            self.stops.load(std::sync::atomic::Ordering::SeqCst)
        }
    }
    #[async_trait]
    impl Gateway for RecordingGateway {
        fn id(&self) -> &str {
            &self.id
        }
        async fn start(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            self.stops.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if self.fail_stop {
                anyhow::bail!("stop raced a trap/restart");
            }
            Ok(())
        }
        async fn create_workspace(&self, name: &str) -> anyhow::Result<String> {
            Ok(format!("ws-{name}"))
        }
        async fn create_conversation(&self, _w: &str, _t: &str) -> anyhow::Result<String> {
            Ok("conv".to_string())
        }
        async fn post_status(&self, surface: &Surface, _text: &str) -> anyhow::Result<MessageRef> {
            Ok(MessageRef {
                surface: surface.clone(),
                message_id: "m1".to_string(),
            })
        }
        async fn edit_status(&self, _msg: &MessageRef, _text: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_result(&self, _surface: &Surface, _chunks: &[String]) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_error(&self, _surface: &Surface, _message: &str) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _s: &Surface,
            _r: &ApprovalRequest,
        ) -> anyhow::Result<ApprovalDecision> {
            Ok(ApprovalDecision::Cancel)
        }
    }

    fn manifest(id: &str) -> PluginManifest {
        PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: id.to_string(),
            version: String::new(),
            publisher: String::new(),
            description: String::new(),
            homepage: None,
            icon: None,
            categories: vec![],
            slot: None,
            verified: false,
            experimental: false,
            auth: None,
            settings: vec![],
            mcp: vec![],
            extensions: vec![],
            skills: vec![],
            provider: None,
        }
    }

    fn harness_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: Some(Arc::new(FakeHarnessFactory)),
            gateway: None,
            connector: None,
            extension: None,
            provider: None,
            source: PluginSource::Builtin,
        }
    }

    fn gateway_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: Some(Arc::new(FakeGatewayFactory)),
            connector: None,
            extension: None,
            provider: None,
            source: PluginSource::Builtin,
        }
    }

    fn manifest_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: None,
            connector: None,
            extension: None,
            provider: None,
            source: PluginSource::Builtin,
        }
    }

    fn connector_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: None,
            connector: Some(Arc::new(FakeConnector)),
            extension: None,
            provider: None,
            source: PluginSource::Builtin,
        }
    }

    async fn open_settings() -> (SettingsStore, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(Store::open(tmp.path()).await.unwrap());
        (SettingsStore::new(store), tmp)
    }

    #[tokio::test]
    async fn unknown_id_errors() {
        let (settings, _tmp) = open_settings().await;
        let host = PluginHost::new();
        let err = toggle_enabled(&host, &settings, "nope", true)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "unknown plugin: nope");
    }

    #[tokio::test]
    async fn harness_capable_toggle_errors_because_native_is_always_enabled() {
        let (settings, _tmp) = open_settings().await;
        let mut host = PluginHost::new();
        host.add(harness_only("native"));
        let err = toggle_enabled(&host, &settings, "native", false)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "native is always enabled");
    }

    #[tokio::test]
    async fn gateway_capable_toggles_enabled_gateways_csv() {
        let (settings, _tmp) = open_settings().await;
        let mut host = PluginHost::new();
        // A fresh store no longer seeds `enabled_gateways` (the native Discord
        // seed was removed with the native gateway), so this starts empty and
        // proves the CSV add/remove round-trip in isolation.
        host.add(gateway_only("slack"));

        toggle_enabled(&host, &settings, "slack", true)
            .await
            .unwrap();
        assert_eq!(
            settings.get("enabled_gateways").await.unwrap().as_deref(),
            Some("slack")
        );
        toggle_enabled(&host, &settings, "slack", false)
            .await
            .unwrap();
        assert_eq!(
            settings.get("enabled_gateways").await.unwrap().as_deref(),
            Some("")
        );
    }

    #[tokio::test]
    async fn manifest_only_toggle_errors_instead_of_silently_no_opping() {
        let (settings, _tmp) = open_settings().await;
        let mut host = PluginHost::new();
        host.add(manifest_only("anthropic-toggle-test"));

        let err = toggle_enabled(&host, &settings, "anthropic-toggle-test", true)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "anthropic-toggle-test is always available");
        // Confirm it really is a no-op: no `plugin.<id>.enabled` row exists.
        assert_eq!(
            settings
                .get("plugin.anthropic-toggle-test.enabled")
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test]
    async fn experimental_toggle_errors_instead_of_silently_no_opping() {
        let (settings, _tmp) = open_settings().await;
        let mut host = PluginHost::new();
        let mut plugin = manifest_only("zep-toggle-test");
        plugin.manifest.experimental = true;
        host.add(plugin);

        let err = toggle_enabled(&host, &settings, "zep-toggle-test", true)
            .await
            .unwrap_err();
        assert_eq!(
            err.to_string(),
            "zep-toggle-test is experimental — nothing to enable"
        );
    }

    #[tokio::test]
    async fn apply_blocked_denylist_disables_enabled_and_refuses_toggle() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let settings = SettingsStore::new(store.clone());
        let mut host = PluginHost::new();
        host.add(connector_only("acme"));

        // Enable "acme" before it's ever blocked.
        toggle_enabled(&host, &settings, "acme", true)
            .await
            .unwrap();
        assert!(host.is_enabled(&settings, "acme").await.unwrap());

        // The feed now blocks "acme" — seed the cached row Task 3's fetch
        // pipeline would have written.
        store
            .upsert_remote_catalog(&[crate::store::RemoteCatalogRow {
                id: "acme".to_string(),
                manifest_toml: String::new(),
                version: String::new(),
                sequence: 1,
                blocked: true,
                blocked_reason: Some("revoked: compromised".to_string()),
                fetched_at: 0,
            }])
            .await
            .unwrap();

        apply_blocked_denylist(&store, &settings, &host)
            .await
            .unwrap();
        assert_eq!(
            settings
                .get("plugin.acme.enabled")
                .await
                .unwrap()
                .as_deref(),
            Some("false"),
            "apply_blocked_denylist must live-disable an already-enabled blocked plugin"
        );
        assert!(!host.is_enabled(&settings, "acme").await.unwrap());

        let err = toggle_enabled(&host, &settings, "acme", true)
            .await
            .unwrap_err();
        assert!(
            err.to_string().contains("blocked"),
            "re-enabling a blocked plugin must be refused, got: {err}"
        );
        // The redacted revocation reason is preserved and surfaced verbatim, so
        // an operator can read WHY the enable was refused.
        assert!(
            err.to_string().contains("revoked: compromised"),
            "the refusal must carry the redacted revocation reason, got: {err}"
        );
    }

    #[tokio::test]
    async fn stop_revoked_gateways_stops_only_matching_ids_and_is_resilient() {
        // A running gateway ("discord") is revoked; an unrelated running gateway
        // ("slack") is not; a revoked connector id ("acme") has no running
        // gateway at all. The mechanism must gracefully stop ONLY discord,
        // leave slack usable, treat acme as a clean no-op, and never branch on a
        // specific plugin id.
        let discord = Arc::new(RecordingGateway::new("discord"));
        let slack = Arc::new(RecordingGateway::new("slack"));
        let gateways: Vec<Arc<dyn Gateway>> = vec![
            Arc::clone(&discord) as Arc<dyn Gateway>,
            Arc::clone(&slack) as Arc<dyn Gateway>,
        ];

        let revoked: std::collections::HashSet<String> =
            ["discord".to_string(), "acme".to_string()]
                .into_iter()
                .collect();
        let stopped = stop_revoked_gateways(&gateways, &revoked).await;

        assert_eq!(
            discord.stops(),
            1,
            "the revoked, running gateway must be stopped exactly once"
        );
        assert_eq!(
            slack.stops(),
            0,
            "an unrelated gateway must stay running/usable"
        );
        assert_eq!(
            stopped,
            vec!["discord".to_string()],
            "only the revoked, running gateway is reported stopped — the revoked connector id \
             ('acme', no running gateway) is a clean no-op, not an error"
        );
    }

    #[tokio::test]
    async fn stop_revoked_gateways_swallows_a_failing_stop_and_finishes_the_sweep() {
        // A stop that errors (racing a trap/restart) must NOT abort the sweep or
        // propagate: a second revoked gateway after it must still be stopped.
        let flaky = Arc::new(RecordingGateway::failing("discord"));
        let other = Arc::new(RecordingGateway::new("slack"));
        let gateways: Vec<Arc<dyn Gateway>> = vec![
            Arc::clone(&flaky) as Arc<dyn Gateway>,
            Arc::clone(&other) as Arc<dyn Gateway>,
        ];
        let revoked: std::collections::HashSet<String> =
            ["discord".to_string(), "slack".to_string()]
                .into_iter()
                .collect();

        let stopped = stop_revoked_gateways(&gateways, &revoked).await;

        assert_eq!(
            flaky.stops(),
            1,
            "the failing gateway's stop was still attempted"
        );
        assert_eq!(
            other.stops(),
            1,
            "a stop error must not abort the rest of the sweep"
        );
        assert_eq!(
            stopped.len(),
            2,
            "both revoked gateways are reported stopped — stop() aborts the supervisor even when \
             the component-stop errors"
        );
    }

    #[tokio::test]
    async fn connector_only_toggle_still_flips_plugin_enabled_flag() {
        let (settings, _tmp) = open_settings().await;
        let mut host = PluginHost::new();
        host.add(connector_only("acme-toggle-test"));

        toggle_enabled(&host, &settings, "acme-toggle-test", true)
            .await
            .unwrap();
        assert_eq!(
            settings
                .get("plugin.acme-toggle-test.enabled")
                .await
                .unwrap()
                .as_deref(),
            Some("true")
        );
        // Read-back through `is_enabled` too, not just the raw setting.
        assert!(host
            .is_enabled(&settings, "acme-toggle-test")
            .await
            .unwrap());

        toggle_enabled(&host, &settings, "acme-toggle-test", false)
            .await
            .unwrap();
        assert_eq!(
            settings
                .get("plugin.acme-toggle-test.enabled")
                .await
                .unwrap()
                .as_deref(),
            Some("false")
        );
        assert!(!host
            .is_enabled(&settings, "acme-toggle-test")
            .await
            .unwrap());
    }
}

#[cfg(test)]
mod load_skill_pack_plugins_tests {
    use super::*;

    const VALID_MANIFEST: &str = r#"
contract = 1
id = "acme-user"
name = "Acme User Plugin"

[[mcp]]
name = "svc"
transport = "stdio"
command = "acme-mcp"
"#;

    fn write_manifest(plugins_root: &std::path::Path, plugin_dir: &str, toml_str: &str) {
        let dir = plugins_root.join(plugin_dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ryuzi-plugin.toml"), toml_str).unwrap();
    }

    /// The stamp `skills_install::install_plugin_pack` writes into the
    /// plugin directory (snake_case keys — see `SkillInstallProvenance`).
    fn stamp_pack(plugins_root: &std::path::Path, plugin_dir: &str, plugin_id: &str) {
        std::fs::write(
            plugins_root.join(plugin_dir).join(".ryuzi-skill.json"),
            format!(
                r#"{{"source":"https://github.com/acme/pack","plugin_id":"{plugin_id}","installed_at":"2026-07-10T00:00:00.000Z"}}"#
            ),
        )
        .unwrap();
    }

    /// Legacy layout: provenance lives only in a materialized skill dir
    /// under the skills root (installs that predate the plugin-dir stamp).
    fn write_legacy_skills_provenance(skills_root: &std::path::Path, plugin_id: &str) {
        let dir = skills_root.join(format!("{plugin_id}--triage"));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join(".ryuzi-skill.json"),
            format!(
                r#"{{"source":"https://github.com/acme/pack","plugin_id":"{plugin_id}","installed_at":"2026-01-01T00:00:00.000Z"}}"#
            ),
        )
        .unwrap();
    }

    #[test]
    fn stamped_skill_pack_registers_with_skill_pack_source() {
        let plugins_root = tempfile::tempdir().unwrap();
        let skills_root = tempfile::tempdir().unwrap();
        write_manifest(plugins_root.path(), "acme", VALID_MANIFEST);
        stamp_pack(plugins_root.path(), "acme", "acme-user");

        let mut regs = Registries::new();
        load_skill_pack_plugins_from(&mut regs, plugins_root.path(), skills_root.path());

        let plugin = regs
            .plugins
            .get("acme-user")
            .expect("stamped skill pack should register");
        assert!(
            plugin.connector.is_some(),
            "manifest has an [[mcp]] entry, so it should be connector-capable"
        );
        assert_eq!(
            plugin.source,
            PluginSource::SkillPack(plugins_root.path().join("acme")),
            "source should record the manifest's own directory"
        );
    }

    #[test]
    fn legacy_pack_with_skills_root_provenance_loads_and_gets_stamped() {
        let plugins_root = tempfile::tempdir().unwrap();
        let skills_root = tempfile::tempdir().unwrap();
        // The heal only trusts a directory whose name equals the manifest's
        // plugin id (see `stamp_legacy_skill_pack_provenance`), matching
        // `install_plugin_pack`'s invariant that packs always live at
        // `plugins_root/<plugin_id>` — so this legacy-layout fixture uses
        // "acme-user" for both the directory and the manifest id.
        write_manifest(plugins_root.path(), "acme-user", VALID_MANIFEST);
        write_legacy_skills_provenance(skills_root.path(), "acme-user");

        let mut regs = Registries::new();
        load_skill_pack_plugins_from(&mut regs, plugins_root.path(), skills_root.path());

        assert!(
            regs.plugins.get("acme-user").is_some(),
            "legacy pack must load"
        );
        assert!(
            plugins_root
                .path()
                .join("acme-user/.ryuzi-skill.json")
                .is_file(),
            "one-time heal must stamp the plugin directory"
        );
    }

    #[test]
    fn legacy_heal_rejects_dir_name_spoofing_an_installed_id() {
        // A hand-authored directory named anything other than the manifest's
        // plugin id must not be healed or loaded, even when it claims a real
        // installed pack's id and that id has genuine materialized
        // skills-root provenance — otherwise a spoofed manifest could ride
        // another pack's provenance to get itself permanently trusted.
        let plugins_root = tempfile::tempdir().unwrap();
        let skills_root = tempfile::tempdir().unwrap();
        write_manifest(plugins_root.path(), "impostor", VALID_MANIFEST);
        write_legacy_skills_provenance(skills_root.path(), "acme-user");

        let mut regs = Registries::new();
        load_skill_pack_plugins_from(&mut regs, plugins_root.path(), skills_root.path());

        assert!(
            regs.plugins.get("acme-user").is_none(),
            "dir name mismatching the claimed plugin id must not be healed or loaded"
        );
        assert!(
            !plugins_root
                .path()
                .join("impostor/.ryuzi-skill.json")
                .is_file(),
            "the impostor directory must not receive a provenance stamp"
        );
    }

    #[test]
    fn hand_authored_manifest_without_provenance_is_skipped() {
        let plugins_root = tempfile::tempdir().unwrap();
        let skills_root = tempfile::tempdir().unwrap();
        write_manifest(plugins_root.path(), "acme", VALID_MANIFEST);

        let mut regs = Registries::new();
        load_skill_pack_plugins_from(&mut regs, plugins_root.path(), skills_root.path());

        assert!(
            regs.plugins.get("acme-user").is_none(),
            "no stamp and no skills-root provenance means the directory is skipped"
        );
    }

    #[test]
    fn broken_toml_is_skipped_without_panicking_and_other_packs_still_load() {
        let plugins_root = tempfile::tempdir().unwrap();
        let skills_root = tempfile::tempdir().unwrap();
        write_manifest(plugins_root.path(), "broken", "this is not valid toml {{{");
        write_manifest(plugins_root.path(), "acme", VALID_MANIFEST);
        stamp_pack(plugins_root.path(), "acme", "acme-user");

        let mut regs = Registries::new();
        load_skill_pack_plugins_from(&mut regs, plugins_root.path(), skills_root.path());

        assert!(
            regs.plugins.get("acme-user").is_some(),
            "the well-formed sibling pack should still load"
        );
        assert_eq!(
            regs.plugins.list().len(),
            1,
            "the broken manifest must not register anything"
        );
    }

    #[test]
    fn manifest_id_colliding_with_a_builtin_is_skipped_by_add_plugin() {
        let plugins_root = tempfile::tempdir().unwrap();
        let skills_root = tempfile::tempdir().unwrap();
        write_manifest(
            plugins_root.path(),
            "fake-anthropic",
            r#"
contract = 1
id = "anthropic"
name = "Fake Anthropic"
"#,
        );
        stamp_pack(plugins_root.path(), "fake-anthropic", "anthropic");

        let mut regs = Registries::new();
        install_builtins(&mut regs); // registers the real "anthropic" provider plugin
        load_skill_pack_plugins_from(&mut regs, plugins_root.path(), skills_root.path());

        let plugin = regs.plugins.get("anthropic").unwrap();
        assert_eq!(
            plugin.source,
            PluginSource::Builtin,
            "first registration (the builtin) must win over the colliding pack"
        );
    }
}

#[cfg(test)]
mod install_builtins_tests {
    use super::*;

    #[test]
    fn install_builtins_adds_every_provider_id() {
        let mut regs = Registries::new();
        install_builtins(&mut regs);
        let ids: Vec<String> = regs
            .plugins
            .list()
            .iter()
            .map(|p| p.manifest.id.clone())
            .collect();

        for d in crate::llm_router::registry::CATALOG {
            assert!(
                ids.contains(&d.id.to_string()),
                "missing provider plugin for {}",
                d.id
            );
        }
    }

    // The whole point of `component_catalog`: a WASM bundle under `plugins/`
    // must be enumerable through `list_plugins`, which reads `PluginHost`.
    #[test]
    fn install_builtins_registers_component_bundles() {
        let mut regs = Registries::new();
        install_builtins(&mut regs);
        let ids: Vec<String> = regs
            .plugins
            .list()
            .iter()
            .map(|p| p.manifest.id.clone())
            .collect();

        for id in [
            "github",
            "atlassian",
            "bitbucket",
            "discord",
            "mimo",
            "opencode",
        ] {
            assert!(ids.iter().any(|got| got == id), "`{id}` must be registered");
        }
    }

    // Providers register first and win the id, so a colliding provider
    // component must never displace the builtin's richer provider metadata.
    #[test]
    fn colliding_provider_ids_stay_owned_by_their_builtin() {
        let mut regs = Registries::new();
        install_builtins(&mut regs);
        for id in component_catalog::COMPONENT_BACKED_PROVIDER_IDS {
            let Some(plugin) = regs.plugins.get(id) else {
                continue; // not every excluded id is in the router CATALOG
            };
            assert_ne!(
                plugin.source,
                host::PluginSource::Component,
                "`{id}` must stay owned by its builtin, not the component"
            );
        }
    }

    #[test]
    fn install_builtins_ids_never_collide_with_native() {
        let mut regs = Registries::new();
        regs.add_plugin(crate::harness::native::native_plugin());
        assert_eq!(
            regs.plugins.list().len(),
            1,
            "sanity: one builtin registered before install_builtins"
        );

        install_builtins(&mut regs);

        let ids: Vec<String> = regs
            .plugins
            .list()
            .iter()
            .map(|p| p.manifest.id.clone())
            .collect();
        let unique: std::collections::HashSet<&String> = ids.iter().collect();
        assert_eq!(
            unique.len(),
            ids.len(),
            "duplicate plugin ids after install_builtins: {ids:?}"
        );

        // 1 pre-registered (native) + every provider + every component bundle
        // the provider registry does not already own (`component_catalog`
        // skips those rather than letting `add_plugin` drop them, so nothing
        // is silently lost here).
        let expected = 1
            + crate::llm_router::registry::CATALOG.len()
            + component_catalog::component_catalog_plugins().len();
        assert_eq!(
            ids.len(),
            expected,
            "install_builtins silently dropped a colliding id instead of staying disjoint \
             from the native builtin"
        );
    }
}
