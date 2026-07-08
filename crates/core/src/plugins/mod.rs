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
//! beside their own implementation module — `native`/`claude-code` live
//! beside their harness code in `harness::native`/`harness::acp`; `discord`
//! lives here since `gateway::discord` is data/protocol-only.
//!
//! [`providers`] and [`runtimes_meta`] generate manifest-only plugins from
//! two existing static catalogs (`llm_router::registry::CATALOG` and
//! `runtimes::CATALOG`) rather than hand-authoring one manifest per entry.
//! [`install_builtins`] adds all of them in one call.

pub mod builtin;
pub mod catalog;
pub mod declarative;
pub mod host;
pub mod oauth;
pub mod providers;
pub mod runtimes_meta;

use crate::settings::{csv, SettingsStore};

pub use host::{plugin_field, CorePlugin, PluginHost, PluginSource, Registries};

/// Add every generated manifest-only builtin — every model provider
/// ([`providers::provider_plugins`]) and every CLI agent
/// ([`runtimes_meta::cli_agent_plugins`]) — plus the embedded integration
/// catalog ([`catalog::catalog_plugins`]) to `regs`.
///
/// This deliberately does NOT add `native`, `claude-code`, or `discord`:
/// those carry host-injected config (an ACP adapter descriptor, a gateway
/// factory) that only the composition root can supply, so hosts add them
/// first and call `install_builtins` afterward (see `runtimes_meta`'s module
/// doc for why `native` is skipped from the CLI-agent catalog for exactly
/// this reason — it would otherwise collide with the harness plugin).
///
/// The catalog is added last: `Registries::add_plugin` keeps the first
/// registration for a colliding id, so providers and CLI agents (added
/// above) always win over a same-id catalog entry, and both of those always
/// lose to `native`/`claude-code`/`discord` (added by the composition root
/// before calling this function).
pub fn install_builtins(regs: &mut Registries) {
    for plugin in providers::provider_plugins() {
        regs.add_plugin(plugin);
    }
    for plugin in runtimes_meta::cli_agent_plugins() {
        regs.add_plugin(plugin);
    }
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
/// deliberately never resolves the claude-code ACP sidecar the way
/// `crates/cli/src/main.rs`'s `build_registries` does, which can print a
/// noisy `eprintln!` note on failure (a regression in `config get` output)
/// and, worse, touch the network or the filesystem to download an adapter.
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
    load_user_plugins(&mut regs);
}

/// Discover and register user-authored plugins from
/// `~/.config/ryuzi/plugins/*/ryuzi-plugin.toml`. Call after
/// [`install_builtins`] so a user manifest can never shadow a built-in
/// (`Registries::add_plugin` keeps the first registration for a given id —
/// see `host`'s module doc).
///
/// A missing config directory is not an error (most installs have none). A
/// plugin directory that fails to parse or fails manifest validation is
/// logged via `tracing::warn!` and skipped — never panics, and never stops
/// the rest of the scan.
pub fn load_user_plugins(regs: &mut Registries) {
    let Some(home) = dirs::home_dir() else {
        tracing::warn!("could not resolve home directory — skipping user plugin discovery");
        return;
    };
    load_user_plugins_from(regs, &home.join(".config/ryuzi/plugins"));
}

/// The scan behind [`load_user_plugins`], factored out so tests can pass a
/// tempdir instead of the real config directory.
pub(crate) fn load_user_plugins_from(regs: &mut Registries, base: &std::path::Path) {
    let Ok(entries) = std::fs::read_dir(base) else {
        return; // no user plugin directory — nothing to do
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
                    "skipping user plugin at {}: invalid manifest: {e}",
                    manifest_path.display()
                );
                continue;
            }
        };
        match declarative::declarative_plugin(manifest, PluginSource::User(dir.clone())) {
            Ok(plugin) => regs.add_plugin(plugin),
            Err(e) => {
                tracing::warn!("skipping user plugin at {}: {e}", manifest_path.display());
            }
        }
    }
}

/// Toggle `id`'s enablement — the single source of truth shared by `ryuzi
/// plugins enable/disable` (`crates/cli/src/plugins_cmd.rs`) and the Cockpit
/// `set_plugin_enabled` command, so the write side can never drift from
/// [`PluginHost::is_enabled`]'s read side:
/// - unknown id → an error (`"unknown plugin: {id}"`)
/// - harness-capable → add/remove `id` in the `enabled_runtimes` CSV setting
/// - gateway-capable → add/remove `id` in the `enabled_gateways` CSV setting
/// - experimental (docs-only, no capability) → an error, since
///   `is_enabled` always reports it disabled regardless of any
///   `plugin.<id>.enabled` write (see that method's doc) — toggling would
///   silently no-op
/// - no harness/gateway/connector capability (manifest-only, e.g. a
///   provider/cli-agent metadata entry) → an error, since `is_enabled`
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
    if plugin.harness.is_some() {
        return toggle_csv(settings, "enabled_runtimes", id, enable).await;
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
            verified: false,
            experimental: false,
            auth: None,
            settings: vec![],
            mcp: vec![],
            skills: vec![],
            menu: None,
            provider: None,
            runtime: None,
        }
    }

    fn harness_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: Some(Arc::new(FakeHarnessFactory)),
            gateway: None,
            connector: None,
            source: PluginSource::Builtin,
        }
    }

    fn gateway_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: Some(Arc::new(FakeGatewayFactory)),
            connector: None,
            source: PluginSource::Builtin,
        }
    }

    fn manifest_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: None,
            connector: None,
            source: PluginSource::Builtin,
        }
    }

    fn connector_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: None,
            connector: Some(Arc::new(FakeConnector)),
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
    async fn harness_capable_toggles_enabled_runtimes_csv_without_duplicating() {
        let (settings, _tmp) = open_settings().await;
        let mut host = PluginHost::new();
        // Deliberately not "native" — a fresh store seeds
        // `enabled_runtimes = "native"`, which would defeat the "off by
        // default" half of this test.
        host.add(harness_only("claude-code"));

        toggle_enabled(&host, &settings, "claude-code", true)
            .await
            .unwrap();
        assert_eq!(
            settings.get("enabled_runtimes").await.unwrap().as_deref(),
            Some("native,claude-code")
        );
        // enabling again must not duplicate the entry
        toggle_enabled(&host, &settings, "claude-code", true)
            .await
            .unwrap();
        assert_eq!(
            settings.get("enabled_runtimes").await.unwrap().as_deref(),
            Some("native,claude-code")
        );
        toggle_enabled(&host, &settings, "claude-code", false)
            .await
            .unwrap();
        assert_eq!(
            settings.get("enabled_runtimes").await.unwrap().as_deref(),
            Some("native")
        );
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
mod load_user_plugins_tests {
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

    fn write_manifest(base: &std::path::Path, plugin_dir: &str, toml_str: &str) {
        let dir = base.join(plugin_dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("ryuzi-plugin.toml"), toml_str).unwrap();
    }

    #[test]
    fn valid_user_manifest_registers_a_connector_capable_plugin_with_user_source() {
        let base = tempfile::tempdir().unwrap();
        write_manifest(base.path(), "acme", VALID_MANIFEST);

        let mut regs = Registries::new();
        load_user_plugins_from(&mut regs, base.path());

        let plugin = regs
            .plugins
            .get("acme-user")
            .expect("valid user manifest should register a plugin");
        assert!(
            plugin.connector.is_some(),
            "manifest has an [[mcp]] entry, so it should be connector-capable"
        );
        assert_eq!(
            plugin.source,
            PluginSource::User(base.path().join("acme")),
            "source should record the manifest's own directory"
        );
    }

    #[test]
    fn broken_toml_is_skipped_without_panicking_and_other_plugins_still_load() {
        let base = tempfile::tempdir().unwrap();
        write_manifest(base.path(), "broken", "this is not valid toml {{{");
        write_manifest(base.path(), "acme", VALID_MANIFEST);

        let mut regs = Registries::new();
        load_user_plugins_from(&mut regs, base.path());

        assert!(
            regs.plugins.get("acme-user").is_some(),
            "the well-formed sibling manifest should still load"
        );
        assert_eq!(
            regs.plugins.list().len(),
            1,
            "the broken manifest must not register anything"
        );
    }

    #[test]
    fn manifest_id_colliding_with_a_builtin_is_skipped_by_add_plugin() {
        let base = tempfile::tempdir().unwrap();
        write_manifest(
            base.path(),
            "fake-anthropic",
            r#"
contract = 1
id = "anthropic"
name = "Fake Anthropic"
"#,
        );

        let mut regs = Registries::new();
        install_builtins(&mut regs); // registers the real "anthropic" provider plugin
        load_user_plugins_from(&mut regs, base.path());

        let plugin = regs.plugins.get("anthropic").unwrap();
        assert_eq!(
            plugin.source,
            PluginSource::Builtin,
            "first registration (the builtin) must win over the colliding user plugin"
        );
    }
}

#[cfg(test)]
mod install_builtins_tests {
    use super::*;
    use crate::harness::acp::AcpAdapterDescriptor;

    fn stub_descriptor() -> AcpAdapterDescriptor {
        AcpAdapterDescriptor {
            command: "true".to_string(),
            args: vec![],
            env: vec![],
            env_remove: vec![],
        }
    }

    #[test]
    fn install_builtins_adds_every_provider_and_cli_agent_id() {
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
        for d in crate::runtimes::CATALOG {
            if d.id == crate::harness::native::NATIVE_ID
                || crate::llm_router::registry::descriptor(d.id).is_some()
            {
                continue; // claimed elsewhere — see `runtimes_meta`'s module doc
            }
            assert!(
                ids.contains(&d.id.to_string()),
                "missing cli-agent plugin for {}",
                d.id
            );
        }
    }

    #[test]
    fn install_builtins_ids_never_collide_with_native_claude_code_or_discord() {
        let mut regs = Registries::new();
        regs.add_plugin(crate::harness::native::native_plugin());
        regs.add_plugin(crate::harness::acp::claude_code_plugin(stub_descriptor()));
        regs.add_plugin(builtin::discord_plugin());
        assert_eq!(
            regs.plugins.list().len(),
            3,
            "sanity: three builtins registered before install_builtins"
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

        // 3 pre-registered (native, claude-code, discord) + every provider +
        // every runtimes-catalog entry EXCEPT `native` (already registered
        // above, under the same id, by the harness plugin) and `ollama`
        // (already covered by the `ollama` model-provider plugin — see
        // `runtimes_meta`'s module doc) + every embedded integration-catalog
        // entry (`catalog::CATALOG_MANIFESTS`, disjoint from all of the
        // above by construction — see `catalog`'s own collision test).
        let expected = 3
            + crate::llm_router::registry::CATALOG.len()
            + (crate::runtimes::CATALOG.len() - 2)
            + catalog::CATALOG_MANIFESTS.len();
        assert_eq!(
            ids.len(),
            expected,
            "install_builtins silently dropped a colliding id instead of staying disjoint \
             from the native/claude-code/discord builtins"
        );
    }
}
