//! `CorePlugin`/`PluginHost`: binds a `PluginManifest` to the runtime
//! capabilities it provides, and `Registries`, the composition root for all
//! three extension axes plus the plugin host itself.
//!
//! This replaces the old `Integration` trait (`crate::integration`, deleted).
//! Previously a host object implemented `Integration` and answered
//! `harness()`/`gateway()`/`connector()` by method; now every extension point
//! is a manifest (`CorePlugin.manifest`) paired with typed `Option<Arc<dyn
//! _>>` capability fields — the manifest is the thing a catalog/user plugin
//! can actually author (as TOML), while a Rust built-in constructs both the
//! manifest and its capabilities in code (see `plugins::builtin` and
//! `harness::{native, acp}`).
//!
//! `Registries::add_plugin` is the one place a `CorePlugin` becomes "live":
//! it fans `harness`/`gateway` into the matching registry under
//! `manifest.id`, AND records the plugin in `self.plugins` so `PluginHost`
//! can answer identity/enablement questions later (e.g. for a settings UI).
//! `connector` is deliberately NOT fanned into `ConnectorRegistry` here — a
//! `CorePlugin` carries a live `Arc<dyn Connector>` instance (not a
//! `ConnectorFactory`), consumed directly from the host by
//! `ControlPlane::start_harness_session` (`control::lifecycle`), which
//! attaches every enabled connector-capable plugin's MCP servers to the
//! session.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, RwLock};

use ryuzi_plugin_sdk::{FieldKind, PluginManifest, SettingField};

use crate::connector::{Connector, ConnectorRegistry};
use crate::gateway::{GatewayFactory, GatewayRegistry};
use crate::harness::{HarnessFactory, HarnessRegistry};
use crate::settings::{csv, SettingsStore};

/// Process-wide registry of every `plugin.*` settings key any installed
/// plugin has declared, populated by [`PluginHost::add`]. Backs
/// `crate::settings::store::validate_setting`'s acceptance of `plugin.*`
/// keys and `crate::settings::catalog::is_secret`'s secret-flagging for
/// plugin-owned fields.
///
/// Global rather than threaded through `SettingsStore`/`validate_setting`
/// because validation is a free function called from many places (CLI,
/// Tauri commands, the settings store itself) that don't otherwise carry a
/// `Registries` handle. Registration is add-only and idempotent — the same
/// key registered twice (e.g. across two `PluginHost`s in different tests)
/// simply overwrites; tests that care about isolation use unique plugin ids.
static PLUGIN_FIELDS: OnceLock<RwLock<HashMap<String, SettingField>>> = OnceLock::new();

fn plugin_fields() -> &'static RwLock<HashMap<String, SettingField>> {
    PLUGIN_FIELDS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Look up a `plugin.*` settings field previously registered by
/// [`PluginHost::add`] — `None` if no installed plugin declares `key`.
pub fn plugin_field(key: &str) -> Option<SettingField> {
    plugin_fields()
        .read()
        .expect("PLUGIN_FIELDS lock poisoned")
        .get(key)
        .cloned()
}

/// Register every `plugin.*` settings key `manifest` declares:
/// - each `manifest.settings[]` field, verbatim
/// - `manifest.auth.setting`, if present, as a synthetic secret `String`
///   field (the manifest's `[auth]` block only names the key; it has no
///   label/help of its own)
/// - `plugin.<id>.enabled`, always, as a `Bool` field — this is what makes
///   `validate_setting("plugin.<id>.enabled", ...)` accept every installed
///   plugin, not just connector-capable ones (harmless for the others: they
///   just never read the key back via `is_enabled`)
fn register_plugin_fields(manifest: &PluginManifest) {
    let mut fields = plugin_fields()
        .write()
        .expect("PLUGIN_FIELDS lock poisoned");
    for f in &manifest.settings {
        fields.insert(f.key.clone(), f.clone());
    }
    if let Some(auth) = &manifest.auth {
        if let Some(key) = &auth.setting {
            fields.insert(
                key.clone(),
                SettingField {
                    key: key.clone(),
                    label: format!("{} auth", manifest.name),
                    help: String::new(),
                    secret: true,
                    required: false,
                    kind: FieldKind::String,
                },
            );
        }
    }
    let enabled_key = format!("plugin.{}.enabled", manifest.id);
    fields.insert(
        enabled_key.clone(),
        SettingField {
            key: enabled_key,
            label: format!("Enable {}", manifest.name),
            help: String::new(),
            secret: false,
            required: false,
            kind: FieldKind::Bool,
        },
    );
}

/// Where a plugin's manifest/behavior came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PluginSource {
    /// Shipped inside the `ryuzi` binary (native, claude-code, discord).
    Builtin,
    /// Bundled in the embedded plugin catalog.
    Catalog,
    /// Loaded from a user-authored manifest on disk.
    User(std::path::PathBuf),
}

/// A manifest bound to the behavioral capabilities it advertises. Each
/// `Some` axis is wired into the matching registry by
/// [`Registries::add_plugin`].
pub struct CorePlugin {
    pub manifest: PluginManifest,
    pub harness: Option<Arc<dyn HarnessFactory>>,
    pub gateway: Option<Arc<dyn GatewayFactory>>,
    pub connector: Option<Arc<dyn Connector>>,
    pub source: PluginSource,
}

/// Every installed plugin, keyed by `manifest.id`, kept in insertion order.
#[derive(Default)]
pub struct PluginHost {
    order: Vec<Arc<CorePlugin>>,
    by_id: HashMap<String, usize>,
}

impl PluginHost {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a plugin. Returns `false` (and logs a warning) without
    /// installing it if `manifest.id` is already taken — the first
    /// registration for an id wins.
    pub fn add(&mut self, plugin: CorePlugin) -> bool {
        if self.by_id.contains_key(&plugin.manifest.id) {
            tracing::warn!(
                "plugin id `{}` already registered — ignoring duplicate",
                plugin.manifest.id
            );
            return false;
        }
        register_plugin_fields(&plugin.manifest);
        self.by_id
            .insert(plugin.manifest.id.clone(), self.order.len());
        self.order.push(Arc::new(plugin));
        true
    }

    /// Look up an installed plugin by id.
    pub fn get(&self, id: &str) -> Option<Arc<CorePlugin>> {
        self.by_id.get(id).map(|&i| self.order[i].clone())
    }

    /// All installed plugins, in insertion order.
    pub fn list(&self) -> Vec<Arc<CorePlugin>> {
        self.order.clone()
    }

    /// Whether `id` is enabled, in priority order:
    /// - unknown id → `false`
    /// - harness-capable → the `enabled_runtimes` CSV setting contains `id`
    /// - gateway-capable → the `enabled_gateways` CSV setting contains `id`
    /// - manifest-only (no harness/gateway/connector capability) → always
    ///   `true`
    /// - connector-only → the setting `plugin.<id>.enabled == "true"`
    ///   (defaults to `false`)
    pub async fn is_enabled(&self, settings: &SettingsStore, id: &str) -> anyhow::Result<bool> {
        let Some(plugin) = self.get(id) else {
            return Ok(false);
        };
        if plugin.harness.is_some() {
            let enabled = csv(settings.get("enabled_runtimes").await?.as_deref());
            return Ok(enabled.iter().any(|r| r == id));
        }
        if plugin.gateway.is_some() {
            let enabled = csv(settings.get("enabled_gateways").await?.as_deref());
            return Ok(enabled.iter().any(|g| g == id));
        }
        if plugin.connector.is_none() {
            // Manifest-only plugin (e.g. a provider/cli-agent metadata entry
            // with no behavioral capability of its own) — always enabled.
            return Ok(true);
        }
        let key = format!("plugin.{id}.enabled");
        Ok(settings.get(&key).await?.as_deref() == Some("true"))
    }

    /// Every `manifest.skills[].path` (resolved relative to the plugin's own
    /// directory) contributed by an *enabled* [`PluginSource::User`] plugin,
    /// filtered down to directories that actually exist on disk — handed to
    /// the native runtime as `SessionCtx::extra_skill_dirs` so plugin-bundled
    /// skills show up beside the worktree/global ones (see
    /// `harness::native::skills::SkillRegistry::load_with`).
    ///
    /// Builtin/catalog plugins never contribute here: a `SkillDef.path` is
    /// only meaningful relative to a manifest's own directory on disk, which
    /// only `PluginSource::User` carries.
    pub async fn enabled_skill_dirs(&self, settings: &SettingsStore) -> Vec<std::path::PathBuf> {
        let mut dirs = Vec::new();
        for plugin in &self.order {
            let PluginSource::User(base) = &plugin.source else {
                continue;
            };
            if plugin.manifest.skills.is_empty() {
                continue;
            }
            match self.is_enabled(settings, &plugin.manifest.id).await {
                Ok(true) => {}
                _ => continue,
            }
            for skill in &plugin.manifest.skills {
                let dir = base.join(&skill.path);
                if dir.is_dir() {
                    dirs.push(dir);
                }
            }
        }
        dirs
    }
}

/// The three extension registries, plus the plugin host recording every
/// installed [`CorePlugin`].
#[derive(Default)]
pub struct Registries {
    pub harness: HarnessRegistry,
    pub gateway: GatewayRegistry,
    pub connector: ConnectorRegistry,
    pub plugins: PluginHost,
}

impl Registries {
    pub fn new() -> Self {
        Registries::default()
    }

    /// Install a plugin: fan its harness/gateway capabilities into the
    /// matching registry under `manifest.id`, and record it in
    /// `self.plugins`. A duplicate `manifest.id` is rejected entirely (no
    /// registry is touched) — see [`PluginHost::add`].
    pub fn add_plugin(&mut self, plugin: CorePlugin) {
        let id = plugin.manifest.id.clone();
        if self.plugins.get(&id).is_some() {
            tracing::warn!("plugin id `{id}` already registered — ignoring duplicate");
            return;
        }
        if let Some(h) = &plugin.harness {
            self.harness.register(id.clone(), h.clone());
        }
        if let Some(g) = &plugin.gateway {
            self.gateway.register(id.clone(), g.clone());
        }
        self.plugins.add(plugin);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::ConnectorCtx;
    use crate::domain::{ApprovalDecision, ApprovalRequest, McpServerSpec, Surface};
    use crate::gateway::{Gateway, MessageRef};
    use crate::harness::{Harness, HarnessSession, SessionCtx};
    use crate::store::Store;
    use async_trait::async_trait;

    // ---- minimal fakes for each axis (self-contained to this test module) ----

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

    fn connector_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: None,
            connector: Some(Arc::new(FakeConnector)),
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

    async fn open_settings() -> (Arc<Store>, SettingsStore, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let settings = SettingsStore::new(store.clone());
        (store, settings, tmp)
    }

    // ---------- PluginHost::add/get/list ----------

    #[test]
    fn add_get_list_preserve_insertion_order() {
        let mut host = PluginHost::new();
        assert!(host.add(harness_only("a")));
        assert!(host.add(gateway_only("b")));

        assert_eq!(host.get("a").unwrap().manifest.id, "a");
        assert_eq!(host.get("b").unwrap().manifest.id, "b");
        assert!(host.get("missing").is_none());

        let ids: Vec<String> = host.list().iter().map(|p| p.manifest.id.clone()).collect();
        assert_eq!(ids, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn duplicate_id_is_rejected_and_first_registration_wins() {
        let mut host = PluginHost::new();
        assert!(host.add(harness_only("dup")));
        assert!(
            !host.add(gateway_only("dup")),
            "second registration of the same id must be rejected"
        );

        let kept = host.get("dup").unwrap();
        assert!(kept.harness.is_some(), "the FIRST registration must win");
        assert!(kept.gateway.is_none());
        assert_eq!(host.list().len(), 1);
    }

    // ---------- Registries::add_plugin ----------

    #[test]
    fn add_plugin_fans_harness_and_gateway_into_registries_under_manifest_id() {
        let mut regs = Registries::new();
        regs.add_plugin(harness_only("claude-code"));
        regs.add_plugin(gateway_only("discord"));

        assert!(regs.harness.get("claude-code").is_some());
        assert!(regs.gateway.get("claude-code").is_none());
        assert!(regs.gateway.get("discord").is_some());
        assert!(regs.harness.get("discord").is_none());

        // both recorded in the host too
        assert!(regs.plugins.get("claude-code").is_some());
        assert!(regs.plugins.get("discord").is_some());
        assert_eq!(regs.harness.names(), vec!["claude-code".to_string()]);
        assert_eq!(regs.gateway.names(), vec!["discord".to_string()]);
    }

    #[test]
    fn add_plugin_does_not_fan_connector_into_connector_registry() {
        let mut regs = Registries::new();
        regs.add_plugin(connector_only("notion"));

        assert!(
            regs.connector.get("notion").is_none(),
            "connector must be consumed directly from the host, not fanned into ConnectorRegistry"
        );
        assert!(regs.plugins.get("notion").is_some());
    }

    #[test]
    fn add_plugin_rejects_duplicate_id_without_touching_any_registry() {
        let mut regs = Registries::new();
        regs.add_plugin(harness_only("dup"));
        regs.add_plugin(gateway_only("dup"));

        assert!(regs.harness.get("dup").is_some());
        assert!(
            regs.gateway.get("dup").is_none(),
            "the duplicate's gateway factory must never be registered"
        );
    }

    // ---------- PluginHost::is_enabled ----------

    #[tokio::test]
    async fn is_enabled_unknown_id_is_false() {
        let (_store, settings, _tmp) = open_settings().await;
        let host = PluginHost::new();
        assert!(!host.is_enabled(&settings, "nope").await.unwrap());
    }

    #[tokio::test]
    async fn is_enabled_harness_capability_follows_enabled_runtimes() {
        let (_store, settings, _tmp) = open_settings().await;
        let mut host = PluginHost::new();
        host.add(harness_only("native"));

        assert!(!host.is_enabled(&settings, "native").await.unwrap());
        settings.set("enabled_runtimes", "native").await.unwrap();
        assert!(host.is_enabled(&settings, "native").await.unwrap());
    }

    #[tokio::test]
    async fn is_enabled_gateway_capability_follows_enabled_gateways() {
        let (_store, settings, _tmp) = open_settings().await;
        let mut host = PluginHost::new();
        // Deliberately NOT "discord" — a fresh `Store` seeds
        // `enabled_gateways = "discord"` (see `store.rs`'s migration seed),
        // which would make this id enabled from the start and defeat the
        // "off by default" half of this test.
        host.add(gateway_only("slack"));

        assert!(!host.is_enabled(&settings, "slack").await.unwrap());
        settings.set("enabled_gateways", "slack").await.unwrap();
        assert!(host.is_enabled(&settings, "slack").await.unwrap());
    }

    #[tokio::test]
    async fn is_enabled_manifest_only_plugin_is_always_true() {
        let (_store, settings, _tmp) = open_settings().await;
        let mut host = PluginHost::new();
        host.add(manifest_only("anthropic"));

        assert!(host.is_enabled(&settings, "anthropic").await.unwrap());
    }

    #[tokio::test]
    async fn is_enabled_connector_only_plugin_defaults_false_until_setting_flips_true() {
        let (store, settings, _tmp) = open_settings().await;
        let mut host = PluginHost::new();
        host.add(connector_only("github"));

        assert!(!host.is_enabled(&settings, "github").await.unwrap());

        // `settings.set` would work equally well now that `validate_setting`
        // accepts `plugin.<id>.enabled` (see `settings::store`) — writing the
        // raw row directly here just mirrors the CLI/Tauri path least
        // coupled to that validation.
        store
            .set_setting_raw("plugin.github.enabled", "true")
            .await
            .unwrap();
        assert!(host.is_enabled(&settings, "github").await.unwrap());
    }

    // ---------- PluginHost::enabled_skill_dirs ----------

    #[tokio::test]
    async fn enabled_skill_dirs_returns_existing_paths_for_enabled_user_plugins() {
        let (store, settings, _tmp) = open_settings().await;
        let base = tempfile::tempdir().unwrap();
        let skill_dir = base.path().join("skills/triage");
        std::fs::create_dir_all(&skill_dir).unwrap();

        let plugin = CorePlugin {
            manifest: PluginManifest {
                skills: vec![ryuzi_plugin_sdk::SkillDef {
                    name: "triage".into(),
                    description: String::new(),
                    path: "skills/triage".into(),
                }],
                ..manifest("task7-skill-user")
            },
            harness: None,
            gateway: None,
            connector: Some(Arc::new(FakeConnector)),
            source: PluginSource::User(base.path().to_path_buf()),
        };
        let mut host = PluginHost::new();
        host.add(plugin);

        assert!(
            host.enabled_skill_dirs(&settings).await.is_empty(),
            "connector-only plugin defaults to disabled — its skills must not surface either"
        );

        store
            .set_setting_raw("plugin.task7-skill-user.enabled", "true")
            .await
            .unwrap();
        assert_eq!(host.enabled_skill_dirs(&settings).await, vec![skill_dir]);
    }

    #[tokio::test]
    async fn enabled_skill_dirs_skips_nonexistent_paths_and_non_user_sources() {
        let (store, settings, _tmp) = open_settings().await;
        let base = tempfile::tempdir().unwrap();
        // No directory created for "skills/missing" — the enabled user
        // plugin's declared skill path simply doesn't exist on disk.
        let mut host = PluginHost::new();
        host.add(CorePlugin {
            manifest: PluginManifest {
                skills: vec![ryuzi_plugin_sdk::SkillDef {
                    name: "missing".into(),
                    description: String::new(),
                    path: "skills/missing".into(),
                }],
                ..manifest("task7-skill-missing")
            },
            harness: None,
            gateway: None,
            connector: Some(Arc::new(FakeConnector)),
            source: PluginSource::User(base.path().to_path_buf()),
        });
        store
            .set_setting_raw("plugin.task7-skill-missing.enabled", "true")
            .await
            .unwrap();

        // A manifest-only builtin (always enabled) whose skill path DOES
        // exist on disk must still be skipped: only `PluginSource::User`
        // carries the on-disk directory a relative `SkillDef.path` resolves
        // against.
        let builtin_dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(builtin_dir.path().join("skills/present")).unwrap();
        host.add(CorePlugin {
            manifest: PluginManifest {
                skills: vec![ryuzi_plugin_sdk::SkillDef {
                    name: "present".into(),
                    description: String::new(),
                    path: "skills/present".into(),
                }],
                ..manifest("task7-skill-builtin")
            },
            harness: None,
            gateway: None,
            connector: None,
            source: PluginSource::Builtin,
        });

        assert!(host.enabled_skill_dirs(&settings).await.is_empty());
    }

    #[tokio::test]
    async fn plugin_leaf_skill_dir_loads_into_skill_registry() {
        use crate::harness::native::skills::SkillRegistry;

        let (store, settings, _tmp) = open_settings().await;
        let plugin_base = tempfile::tempdir().unwrap();
        // Create a leaf skill directory (SKILL.md directly inside).
        let skill_dir = plugin_base.path().join("task7fix-skills");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: github-triage\ndescription: Triage GitHub issues\n---\nLabel and assign issues.",
        )
        .unwrap();

        // Create a user plugin that bundles this leaf skill dir.
        let plugin = CorePlugin {
            manifest: PluginManifest {
                skills: vec![ryuzi_plugin_sdk::SkillDef {
                    name: "github-triage".into(),
                    description: "Triage GitHub issues".into(),
                    path: "task7fix-skills".into(),
                }],
                ..manifest("task7fix-plugin")
            },
            harness: None,
            gateway: None,
            connector: Some(Arc::new(FakeConnector)),
            source: PluginSource::User(plugin_base.path().to_path_buf()),
        };

        let mut host = PluginHost::new();
        host.add(plugin);

        // Enable the plugin.
        store
            .set_setting_raw("plugin.task7fix-plugin.enabled", "true")
            .await
            .unwrap();

        // Get enabled skill dirs and load skills.
        let dirs = host.enabled_skill_dirs(&settings).await;
        assert_eq!(dirs.len(), 1);

        let worktree = tempfile::tempdir().unwrap();
        let registry = SkillRegistry::load_with(worktree.path(), &dirs);

        // The skill must be discovered.
        let skill = registry
            .get("github-triage")
            .expect("leaf skill dir must be discovered");
        assert_eq!(skill.description, "Triage GitHub issues");
        assert!(skill.body.contains("Label and assign"));
    }
}
