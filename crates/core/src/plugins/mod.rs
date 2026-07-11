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
//! [`builtin`] holds first-party plugins that don't have a more natural home
//! beside their own implementation module — `native` lives beside its
//! harness code in `harness::native`; `discord` lives here since
//! `gateway::discord` is data/protocol-only.
//!
//! [`providers`] generates manifest-only plugins from the static provider
//! catalog (`llm_router::registry::CATALOG`) rather than hand-authoring one
//! manifest per entry. [`install_builtins`] adds them plus the embedded
//! catalog in one call.

pub mod builtin;
pub mod catalog;
pub mod catalog_feed_key;
pub mod declarative;
pub mod doctor;
pub mod extension;
pub mod host;
pub mod oauth;
pub mod providers;
pub mod remote_catalog;

use crate::settings::{csv, SettingsStore};
use crate::store::Store;

pub use doctor::{plugin_doctor, DoctorFinding};
pub use extension::{
    ExtensionCtx, ExtensionFactory, ExtensionHost, ExtensionProc, ExtensionSpec, ExtensionStatus,
};
pub use host::{plugin_field, plugin_fields_all, CorePlugin, PluginHost, PluginSource, Registries};

/// Add every generated manifest-only builtin — every model provider
/// ([`providers::provider_plugins`]) — to `regs`. Factored out of
/// [`install_builtins`] so the daemon composition root can add providers,
/// then the (embedded + remote-catalog) merged set from
/// [`catalog::merged_catalog_plugins`], in place of the embedded-only
/// `catalog_plugins()` loop `install_builtins` still runs for callers that
/// don't read the remote cache (e.g. `test_cp`, `ryuzi config`).
pub fn install_providers(regs: &mut Registries) {
    for plugin in providers::provider_plugins() {
        regs.add_plugin(plugin);
    }
}

/// Add every generated manifest-only builtin — every model provider
/// ([`providers::provider_plugins`]) — plus the embedded integration catalog
/// ([`catalog::catalog_plugins`]) to `regs`.
///
/// This deliberately does NOT add `native` or `discord`: those carry
/// host-injected config (a gateway factory) that only the composition root
/// can supply, so hosts add them first and call `install_builtins`
/// afterward.
///
/// The catalog is added last: `Registries::add_plugin` keeps the first
/// registration for a colliding id, so providers (added above) always win
/// over a same-id catalog entry, and both of those always lose to
/// `native`/`discord` (added by the composition root before calling this
/// function).
///
/// This is the embedded-catalog-only path. The daemon's real composition
/// root (`daemon::build_daemon`) does NOT call this — it calls
/// [`install_providers`] followed by [`catalog::merged_catalog_plugins`] so
/// the remote catalog cache can version-gate override the embedded catalog.
/// `install_builtins` stays embedded-only for every other caller (tests,
/// `ryuzi config`) so their behavior is unaffected by the remote cache.
pub fn install_builtins(regs: &mut Registries) {
    install_providers(regs);
    for plugin in catalog::catalog_plugins() {
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
    regs.add_plugin(builtin::discord_plugin());
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
) -> anyhow::Result<()> {
    let blocked: Vec<String> = store
        .list_remote_catalog()
        .await?
        .into_iter()
        .filter(|r| r.blocked)
        .map(|r| r.id)
        .collect();
    for id in blocked {
        if host.get(&id).is_some() && host.is_enabled(settings, &id).await.unwrap_or(false) {
            match settings.set(&format!("plugin.{id}.enabled"), "false").await {
                Ok(()) => tracing::warn!("catalog: auto-disabled blocked plugin {id}"),
                Err(e) => {
                    tracing::warn!("catalog: failed to auto-disable blocked plugin {id}: {e}")
                }
            }
        }
    }
    Ok(())
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
        // Deliberately not "discord" — a fresh store seeds
        // `enabled_gateways = "discord"`, which would defeat the "off by
        // default" half of this test.
        host.add(gateway_only("slack"));

        toggle_enabled(&host, &settings, "slack", true)
            .await
            .unwrap();
        assert_eq!(
            settings.get("enabled_gateways").await.unwrap().as_deref(),
            Some("discord,slack")
        );
        toggle_enabled(&host, &settings, "slack", false)
            .await
            .unwrap();
        assert_eq!(
            settings.get("enabled_gateways").await.unwrap().as_deref(),
            Some("discord")
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

    #[test]
    fn install_builtins_ids_never_collide_with_native_claude_code_or_discord() {
        let mut regs = Registries::new();
        regs.add_plugin(crate::harness::native::native_plugin());
        regs.add_plugin(builtin::discord_plugin());
        assert_eq!(
            regs.plugins.list().len(),
            2,
            "sanity: two builtins registered before install_builtins"
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

        // 2 pre-registered (native, discord) + every provider + every
        // embedded integration-catalog entry (`catalog::CATALOG_MANIFESTS`,
        // disjoint from all of the above by construction — see `catalog`'s
        // own collision test).
        let expected =
            2 + crate::llm_router::registry::CATALOG.len() + catalog::CATALOG_MANIFESTS.len();
        assert_eq!(
            ids.len(),
            expected,
            "install_builtins silently dropped a colliding id instead of staying disjoint \
             from the native/discord builtins"
        );
    }
}
