//! Plugins screen commands: every installed plugin's identity/capabilities
//! (`list_plugins`), a single plugin's full detail (`plugin_detail`),
//! enable/disable (`set_plugin_enabled` — delegates to
//! [`ryuzi_core::plugins::toggle_enabled`], the same helper `ryuzi plugins
//! enable/disable` uses, so the two surfaces can never drift), a validated
//! settings write (`set_plugin_setting`), and a provider's effective model
//! list (`plugin_models`).
//!
//! DTOs here are deliberate thin mirrors of `ryuzi_plugin_sdk::PluginManifest`
//! (and `ryuzi_core::plugins::CorePlugin`) rather than re-exports: the
//! manifest is the engine's contract for plugin authors, while these shapes
//! are the Cockpit UI's contract, free to add UI-only fields (like
//! `value_set`/`configured` booleans) without perturbing the engine type.
//!
//! Secrets are never returned: `PluginAuthInfo.configured` and
//! `PluginFieldInfo.value_set` are booleans derived from whether a row is
//! persisted (or an auth env var is set), never the value itself.

use crate::error::CmdError;
use ryuzi_core::plugins::providers;
use ryuzi_core::settings::SettingsStore;
use ryuzi_core::{ControlPlane, CorePlugin, PluginSource, Store};
use ryuzi_plugin_sdk::{AuthKind, AuthSpec, McpServerDef, McpTransportDef, SettingField};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: Option<String>,
    pub categories: Vec<String>,
    pub verified: bool,
    pub experimental: bool,
    pub enabled: bool,
    /// `builtin` | `catalog` | `user`.
    pub source: String,
    /// Any of `provider` | `runtime` | `gateway` | `connector`.
    pub capabilities: Vec<String>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginAuthInfo {
    /// `none` | `api-key` | `token` | `oauth`.
    pub kind: String,
    pub setting: Option<String>,
    pub env: Option<String>,
    pub help_url: Option<String>,
    /// A persisted (non-empty) row exists for `setting`, OR `env` is set in
    /// the process environment. Never reveals the value itself.
    pub configured: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginFieldInfo {
    pub key: String,
    pub label: String,
    pub help: String,
    pub secret: bool,
    pub required: bool,
    /// A persisted (non-empty) row exists for `key`. Never the value itself.
    pub value_set: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginMcpInfo {
    pub name: String,
    /// `stdio` | `http`.
    pub transport: String,
    /// The raw manifest string (command for stdio, url for http) — no
    /// `${auth}` substitution, matching `ryuzi plugins info`'s output.
    pub command_or_url: String,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginDetail {
    pub info: PluginInfo,
    pub auth: Option<PluginAuthInfo>,
    pub settings: Vec<PluginFieldInfo>,
    pub mcp: Vec<PluginMcpInfo>,
    pub models: Vec<String>,
    pub menu_label: Option<String>,
    pub homepage: Option<String>,
    pub publisher: String,
}

fn source_label(source: &PluginSource) -> &'static str {
    match source {
        PluginSource::Builtin => "builtin",
        PluginSource::Catalog => "catalog",
        PluginSource::User(_) => "user",
    }
}

fn plugin_info(plugin: &CorePlugin, enabled: bool) -> PluginInfo {
    let m = &plugin.manifest;
    PluginInfo {
        id: m.id.clone(),
        name: m.name.clone(),
        description: m.description.clone(),
        icon: m.icon.clone(),
        categories: m.categories.clone(),
        verified: m.verified,
        experimental: m.experimental,
        enabled,
        source: source_label(&plugin.source).to_string(),
        capabilities: plugin
            .capabilities()
            .into_iter()
            .map(str::to_string)
            .collect(),
    }
}

fn auth_kind_label(kind: AuthKind) -> &'static str {
    match kind {
        AuthKind::None => "none",
        AuthKind::ApiKey => "api-key",
        AuthKind::Token => "token",
        AuthKind::Oauth => "oauth",
    }
}

/// Whether an auth block's credential is configured: a persisted, non-empty
/// value under `auth.setting`, or — fallback — the `auth.env` var set in the
/// process environment. Pure so it's testable without a `Store` or a real
/// process environment; callers resolve `setting_value`/`env_is_set` first
/// (see `build_auth_info`).
fn auth_configured(setting_value: Option<&str>, env_is_set: bool) -> bool {
    setting_value.is_some_and(|v| !v.is_empty()) || env_is_set
}

async fn build_auth_info(store: &Store, auth: &AuthSpec) -> anyhow::Result<PluginAuthInfo> {
    let setting_value = match &auth.setting {
        Some(key) => store.get_setting_raw(key).await?,
        None => None,
    };
    let env_is_set = auth
        .env
        .as_deref()
        .is_some_and(|e| std::env::var_os(e).is_some());
    Ok(PluginAuthInfo {
        kind: auth_kind_label(auth.kind).to_string(),
        setting: auth.setting.clone(),
        env: auth.env.clone(),
        help_url: auth.help_url.clone(),
        configured: auth_configured(setting_value.as_deref(), env_is_set),
    })
}

/// Whether a settings field's value is set: a persisted, non-empty row.
/// Pure — callers resolve the persisted row first (see `build_settings_info`).
fn field_value_set(persisted: Option<&str>) -> bool {
    persisted.is_some_and(|v| !v.is_empty())
}

async fn build_settings_info(
    store: &Store,
    fields: &[SettingField],
) -> anyhow::Result<Vec<PluginFieldInfo>> {
    let mut out = Vec::with_capacity(fields.len());
    for f in fields {
        let persisted = store.get_setting_raw(&f.key).await?;
        out.push(PluginFieldInfo {
            key: f.key.clone(),
            label: f.label.clone(),
            help: f.help.clone(),
            secret: f.secret,
            required: f.required,
            value_set: field_value_set(persisted.as_deref()),
        });
    }
    Ok(out)
}

fn mcp_transport_label(t: McpTransportDef) -> &'static str {
    match t {
        McpTransportDef::Stdio => "stdio",
        McpTransportDef::Http => "http",
    }
}

/// Raw manifest string, no `${auth}` substitution — command for stdio, url
/// for http.
fn mcp_info(server: &McpServerDef) -> PluginMcpInfo {
    PluginMcpInfo {
        name: server.name.clone(),
        transport: mcp_transport_label(server.transport).to_string(),
        command_or_url: server
            .command
            .clone()
            .or_else(|| server.url.clone())
            .unwrap_or_default(),
    }
}

async fn assemble_list(cp: &ControlPlane) -> anyhow::Result<Vec<PluginInfo>> {
    let settings = SettingsStore::new(cp.store().clone());
    let mut out = Vec::new();
    for plugin in cp.plugins().list() {
        let enabled = cp
            .plugins()
            .is_enabled(&settings, &plugin.manifest.id)
            .await?;
        out.push(plugin_info(&plugin, enabled));
    }
    Ok(out)
}

async fn assemble_detail(cp: &ControlPlane, id: &str) -> anyhow::Result<PluginDetail> {
    let Some(plugin) = cp.plugins().get(id) else {
        anyhow::bail!("unknown plugin: {id}");
    };
    let settings = SettingsStore::new(cp.store().clone());
    let enabled = cp.plugins().is_enabled(&settings, id).await?;
    let m = &plugin.manifest;

    let auth = match &m.auth {
        Some(auth) => Some(build_auth_info(cp.store(), auth).await?),
        None => None,
    };
    let settings_info = build_settings_info(cp.store(), &m.settings).await?;
    let mcp = m.mcp.iter().map(mcp_info).collect();
    let models = providers::list_models(cp.store(), id).await?;

    Ok(PluginDetail {
        info: plugin_info(&plugin, enabled),
        auth,
        settings: settings_info,
        mcp,
        models,
        menu_label: m.menu.as_ref().and_then(|menu| menu.label.clone()),
        homepage: m.homepage.clone(),
        publisher: m.publisher.clone(),
    })
}

#[tauri::command]
#[specta::specta]
pub async fn list_plugins(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<PluginInfo>> {
    Ok(assemble_list(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn plugin_detail(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<PluginDetail> {
    Ok(assemble_detail(&cp, &id).await?)
}

/// Same semantics as `ryuzi plugins enable/disable` — delegates to the
/// shared core helper so the two surfaces never drift.
#[tauri::command]
#[specta::specta]
pub async fn set_plugin_enabled(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    enabled: bool,
) -> R<()> {
    let settings = SettingsStore::new(cp.store().clone());
    ryuzi_core::plugins::toggle_enabled(cp.plugins(), &settings, &id, enabled).await?;
    Ok(())
}

/// Validated write through `SettingsStore::set` — rejects unknown keys and
/// type-mismatched values the same way `ryuzi config set` does. Never
/// returns a value, so no secret can leak back through this command.
#[tauri::command]
#[specta::specta]
pub async fn set_plugin_setting(
    cp: State<'_, Arc<ControlPlane>>,
    key: String,
    value: String,
) -> R<()> {
    let settings = SettingsStore::new(cp.store().clone());
    settings.set(&key, &value).await?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn plugin_models(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<Vec<String>> {
    Ok(providers::list_models(cp.store(), &id).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryuzi_core::connector::{Connector, ConnectorCtx};
    use ryuzi_core::domain::McpServerSpec;
    use ryuzi_core::gateway::{Gateway, GatewayFactory};
    use ryuzi_core::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx};
    use ryuzi_core::Registries;
    use ryuzi_plugin_sdk::{ModelDef, PluginManifest, ProviderMeta, RuntimeMeta};

    // ---- minimal fakes, self-contained to this test module ----

    struct FakeHarness;
    #[async_trait::async_trait]
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
    #[async_trait::async_trait]
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
        async fn post_status(
            &self,
            surface: &ryuzi_core::domain::Surface,
            _text: &str,
        ) -> anyhow::Result<ryuzi_core::gateway::MessageRef> {
            Ok(ryuzi_core::gateway::MessageRef {
                surface: surface.clone(),
                message_id: "m1".to_string(),
            })
        }
        async fn edit_status(
            &self,
            _msg: &ryuzi_core::gateway::MessageRef,
            _text: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_result(
            &self,
            _surface: &ryuzi_core::domain::Surface,
            _chunks: &[String],
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_error(
            &self,
            _surface: &ryuzi_core::domain::Surface,
            _message: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _s: &ryuzi_core::domain::Surface,
            _r: &ryuzi_core::ApprovalRequest,
        ) -> anyhow::Result<ryuzi_core::ApprovalDecision> {
            Ok(ryuzi_core::ApprovalDecision::Cancel)
        }
    }
    struct FakeGatewayFactory;
    impl GatewayFactory for FakeGatewayFactory {
        fn create(&self, _c: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>> {
            Ok(Arc::new(FakeGateway))
        }
    }

    struct FakeConnector;
    #[async_trait::async_trait]
    impl Connector for FakeConnector {
        async fn mcp_servers(&self, _ctx: &ConnectorCtx) -> anyhow::Result<Vec<McpServerSpec>> {
            Ok(vec![])
        }
    }

    fn manifest(id: &str) -> PluginManifest {
        PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: format!("Plugin {id}"),
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
            source: PluginSource::Catalog,
        }
    }

    fn connector_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: None,
            connector: Some(Arc::new(FakeConnector)),
            source: PluginSource::User(std::path::PathBuf::from("/tmp/whatever")),
        }
    }

    fn manifest_only_with_runtime_meta(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: PluginManifest {
                runtime: Some(RuntimeMeta {
                    binary: Some("acme".to_string()),
                    npm_package: None,
                    default_model: None,
                }),
                ..manifest(id)
            },
            harness: None,
            gateway: None,
            connector: None,
            source: PluginSource::Builtin,
        }
    }

    fn provider_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: PluginManifest {
                provider: Some(ProviderMeta {
                    format: "openai".to_string(),
                    base_url: None,
                    models: vec![ModelDef {
                        id: "m1".to_string(),
                        label: None,
                        default: true,
                    }],
                }),
                ..manifest(id)
            },
            harness: None,
            gateway: None,
            connector: None,
            source: PluginSource::Builtin,
        }
    }

    // ---------- capabilities ----------

    #[test]
    fn capabilities_provider_from_manifest() {
        assert_eq!(provider_only("p").capabilities(), vec!["provider"]);
    }

    #[test]
    fn capabilities_runtime_from_live_harness() {
        assert_eq!(harness_only("h").capabilities(), vec!["runtime"]);
    }

    #[test]
    fn capabilities_runtime_from_manifest_only_runtime_meta() {
        assert_eq!(
            manifest_only_with_runtime_meta("r").capabilities(),
            vec!["runtime"]
        );
    }

    #[test]
    fn capabilities_gateway_from_live_gateway() {
        assert_eq!(gateway_only("g").capabilities(), vec!["gateway"]);
    }

    #[test]
    fn capabilities_connector_from_live_connector() {
        assert_eq!(connector_only("c").capabilities(), vec!["connector"]);
    }

    #[test]
    fn capabilities_empty_for_manifest_only_plugin() {
        assert!(CorePlugin {
            manifest: manifest("m"),
            harness: None,
            gateway: None,
            connector: None,
            source: PluginSource::Builtin,
        }
        .capabilities()
        .is_empty());
    }

    // ---------- source_label ----------

    #[test]
    fn source_label_maps_every_variant() {
        assert_eq!(source_label(&PluginSource::Builtin), "builtin");
        assert_eq!(source_label(&PluginSource::Catalog), "catalog");
        assert_eq!(
            source_label(&PluginSource::User(std::path::PathBuf::from("/x"))),
            "user"
        );
    }

    // ---------- plugin_info ----------

    #[test]
    fn plugin_info_maps_identity_and_enabled_flag_through() {
        let plugin = harness_only("native");
        let info = plugin_info(&plugin, true);
        assert_eq!(info.id, "native");
        assert_eq!(info.name, "Plugin native");
        assert!(info.enabled);
        assert_eq!(info.source, "builtin");
        assert_eq!(info.capabilities, vec!["runtime".to_string()]);

        let info_disabled = plugin_info(&plugin, false);
        assert!(!info_disabled.enabled);
    }

    // ---------- auth_kind_label / auth_configured ----------

    #[test]
    fn auth_kind_label_maps_every_variant() {
        assert_eq!(auth_kind_label(AuthKind::None), "none");
        assert_eq!(auth_kind_label(AuthKind::ApiKey), "api-key");
        assert_eq!(auth_kind_label(AuthKind::Token), "token");
        assert_eq!(auth_kind_label(AuthKind::Oauth), "oauth");
    }

    #[test]
    fn auth_configured_true_when_setting_value_is_non_empty() {
        assert!(auth_configured(Some("sk-secret"), false));
    }

    #[test]
    fn auth_configured_true_when_env_fallback_is_set() {
        assert!(auth_configured(None, true));
        assert!(auth_configured(Some(""), true));
    }

    #[test]
    fn auth_configured_false_when_neither_setting_nor_env_present() {
        assert!(!auth_configured(None, false));
        assert!(!auth_configured(Some(""), false));
    }

    // ---------- field_value_set ----------

    #[test]
    fn field_value_set_true_only_for_non_empty_persisted_value() {
        assert!(field_value_set(Some("x")));
        assert!(!field_value_set(Some("")));
        assert!(!field_value_set(None));
    }

    // ---------- mcp_transport_label / mcp_info ----------

    #[test]
    fn mcp_transport_label_maps_both_variants() {
        assert_eq!(mcp_transport_label(McpTransportDef::Stdio), "stdio");
        assert_eq!(mcp_transport_label(McpTransportDef::Http), "http");
    }

    #[test]
    fn mcp_info_uses_command_for_stdio_and_url_for_http() {
        let stdio = McpServerDef {
            name: "svc".to_string(),
            transport: McpTransportDef::Stdio,
            command: Some("npx".to_string()),
            args: vec![],
            env: Default::default(),
            url: None,
            headers: Default::default(),
        };
        let info = mcp_info(&stdio);
        assert_eq!(info.transport, "stdio");
        assert_eq!(info.command_or_url, "npx");

        let http = McpServerDef {
            name: "svc2".to_string(),
            transport: McpTransportDef::Http,
            command: None,
            args: vec![],
            env: Default::default(),
            url: Some("https://example.com/mcp".to_string()),
            headers: Default::default(),
        };
        let info2 = mcp_info(&http);
        assert_eq!(info2.transport, "http");
        assert_eq!(info2.command_or_url, "https://example.com/mcp");
    }

    // ---------- assemble_list / assemble_detail (ControlPlane-backed) ----------

    async fn test_cp() -> Arc<ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = ryuzi_core::Store::open(tmp.path()).await.unwrap();
        let mut regs = Registries::new();
        ryuzi_core::plugins::install_builtins(&mut regs);
        ControlPlane::new(store, regs).await
    }

    #[tokio::test]
    async fn list_includes_anthropic_enabled_with_provider_capability() {
        let cp = test_cp().await;
        let list = assemble_list(&cp).await.unwrap();
        let anthropic = list
            .iter()
            .find(|p| p.id == "anthropic")
            .expect("anthropic plugin present");
        assert!(
            anthropic.enabled,
            "manifest-only plugins are always enabled"
        );
        assert_eq!(anthropic.capabilities, vec!["provider".to_string()]);
        assert_eq!(anthropic.source, "builtin");
    }

    #[tokio::test]
    async fn detail_unknown_id_errors() {
        let cp = test_cp().await;
        match assemble_detail(&cp, "nope").await {
            Ok(_) => panic!("expected an error for an unknown plugin id"),
            Err(e) => assert_eq!(e.to_string(), "unknown plugin: nope"),
        }
    }

    #[tokio::test]
    async fn detail_anthropic_has_provider_models_and_unconfigured_api_key_auth() {
        let cp = test_cp().await;
        let detail = assemble_detail(&cp, "anthropic").await.unwrap();
        assert_eq!(detail.info.id, "anthropic");
        assert!(!detail.models.is_empty());
        assert!(detail.settings.is_empty());
        assert!(detail.mcp.is_empty());
        assert_eq!(detail.publisher, "ryuzi");

        let auth = detail
            .auth
            .expect("anthropic manifest declares an auth block");
        assert_eq!(auth.kind, "api-key");
        assert!(
            !auth.configured,
            "no connection/env configured in a fresh store"
        );
    }

    #[tokio::test]
    async fn set_plugin_enabled_and_setting_round_trip_through_the_control_plane() {
        let cp = test_cp().await;
        let settings = SettingsStore::new(cp.store().clone());

        // anthropic is a manifest-only plugin (no harness/gateway/connector
        // capability): `is_enabled` always reports it enabled regardless of
        // any `plugin.<id>.enabled` write, so toggling it must error rather
        // than silently no-op (see `toggle_enabled`'s doc).
        let err = ryuzi_core::plugins::toggle_enabled(cp.plugins(), &settings, "anthropic", true)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "anthropic is always available");

        settings
            .set("default_perm_mode", "acceptEdits")
            .await
            .unwrap();
        assert_eq!(
            settings.get("default_perm_mode").await.unwrap().as_deref(),
            Some("acceptEdits")
        );
    }
}
