//! Model-provider plugins: every entry in `llm_router::registry::CATALOG`
//! surfaced as a manifest-only [`CorePlugin`] — no harness/gateway/connector
//! capability of its own. Credentials and per-connection routing stay owned
//! by `llm_router::connections`; the manifest exists purely so a provider
//! shows up in the plugin host/catalog UI alongside real plugins.

use ryuzi_plugin_sdk::{
    AuthKind, AuthSpec, ModelDef, PluginManifest, ProviderMeta, CONTRACT_VERSION,
};

use crate::llm_router::connections::{self, effective_models};
use crate::llm_router::registry::{
    descriptor, ApiFormat, AuthScheme, ProviderCategory, ProviderDescriptor, CATALOG,
};
use crate::store::Store;

use super::host::{CorePlugin, PluginSource};

/// The extra (non-`model-provider`) category label carried alongside
/// `"model-provider"` — mirrors `ProviderCategory` so the catalog UI can
/// grey out OAuth/Free entries the same way `llm_router` does.
fn category_label(category: ProviderCategory) -> &'static str {
    match category {
        ProviderCategory::ApiKey => "api-key",
        ProviderCategory::OAuth => "oauth",
        ProviderCategory::Free => "free",
    }
}

fn category_readable(category: ProviderCategory) -> &'static str {
    match category {
        ProviderCategory::ApiKey => "API key",
        ProviderCategory::OAuth => "OAuth",
        ProviderCategory::Free => "free",
    }
}

fn format_label(format: ApiFormat) -> &'static str {
    match format {
        ApiFormat::Anthropic => "anthropic",
        ApiFormat::OpenAi => "openai",
    }
}

/// Map the wire auth scheme onto the SDK's `AuthSpec`. `setting`/`env` stay
/// `None`: unlike a connector plugin, a provider's credential lives on the
/// `llm_router::connections` row the user creates, not on the plugin itself.
fn auth_spec(auth: AuthScheme) -> AuthSpec {
    let kind = match auth {
        AuthScheme::XApiKey | AuthScheme::Bearer => AuthKind::ApiKey,
        AuthScheme::None => AuthKind::None,
    };
    AuthSpec {
        kind,
        setting: None,
        env: None,
        help_url: None,
    }
}

/// The descriptor's seed model list, as `ModelDef`s — the first model (if
/// any) is marked `default`, matching the "first entry is the default" shape
/// the rest of the catalog assumes.
fn models_to_defs(models: &[&'static str]) -> Vec<ModelDef> {
    models
        .iter()
        .enumerate()
        .map(|(i, id)| ModelDef {
            id: (*id).to_string(),
            label: None,
            default: i == 0,
        })
        .collect()
}

fn provider_plugin(d: &ProviderDescriptor) -> CorePlugin {
    CorePlugin {
        manifest: PluginManifest {
            contract: CONTRACT_VERSION,
            id: d.id.to_string(),
            name: d.name.to_string(),
            version: "0.0.0".to_string(),
            publisher: "ryuzi".to_string(),
            description: format!(
                "{} — model provider ({}) routed through the local endpoint.",
                d.name,
                category_readable(d.category)
            ),
            homepage: None,
            icon: None,
            categories: vec![
                "model-provider".to_string(),
                category_label(d.category).to_string(),
            ],
            verified: true,
            experimental: false,
            auth: Some(auth_spec(d.auth)),
            settings: vec![],
            mcp: vec![],
            skills: vec![],
            menu: None,
            provider: Some(ProviderMeta {
                format: format_label(d.format).to_string(),
                base_url: d.base_url.map(|s| s.to_string()),
                models: models_to_defs(d.models),
            }),
            runtime: None,
        },
        harness: None,
        gateway: None,
        connector: None,
        source: PluginSource::Builtin,
    }
}

/// Every `llm_router::registry::CATALOG` provider as a manifest-only plugin.
pub fn provider_plugins() -> Vec<CorePlugin> {
    CATALOG.iter().map(provider_plugin).collect()
}

/// `id`'s effective model list: the manifest's seed models
/// (`registry::descriptor(id).models`), replaced by a connection's
/// `models_override` when one exists for that provider id (first match, in
/// the same priority order `connections::list_connections` returns). An
/// unknown provider id yields an empty list rather than an error.
pub async fn list_models(store: &Store, id: &str) -> anyhow::Result<Vec<String>> {
    let Some(desc) = descriptor(id) else {
        return Ok(vec![]);
    };
    let conns = connections::list_connections(store).await?;
    for conn in conns
        .iter()
        .filter(|c| c.provider == id)
        .filter(|c| c.enabled)
    {
        if conn
            .data
            .models_override
            .as_ref()
            .is_some_and(|m| !m.is_empty())
        {
            return Ok(effective_models(desc, conn));
        }
    }
    Ok(desc.models.iter().map(|s| s.to_string()).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::connections::{add_connection, ConnectionData, ConnectionRow};

    async fn mem_store() -> Store {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        // Keep the file alive by leaking the handle for the test's duration.
        let (_, path) = tmp.keep().unwrap();
        Store::open(&path).await.unwrap()
    }

    #[test]
    fn every_catalog_provider_is_mapped() {
        let plugins = provider_plugins();
        assert_eq!(plugins.len(), CATALOG.len());
        for d in CATALOG {
            assert!(
                plugins.iter().any(|p| p.manifest.id == d.id),
                "missing provider plugin for {}",
                d.id
            );
        }
    }

    #[test]
    fn anthropic_manifest_has_expected_shape() {
        let plugin = provider_plugins()
            .into_iter()
            .find(|p| p.manifest.id == "anthropic")
            .expect("anthropic plugin");

        assert!(plugin.manifest.warnings().is_empty());
        assert_eq!(plugin.manifest.contract, CONTRACT_VERSION);
        assert_eq!(plugin.manifest.name, "Anthropic");
        assert_eq!(plugin.manifest.publisher, "ryuzi");
        assert!(plugin.manifest.verified);
        assert_eq!(
            plugin.manifest.categories,
            vec!["model-provider".to_string(), "api-key".to_string()]
        );

        let provider = plugin.manifest.provider.expect("provider block");
        assert_eq!(provider.format, "anthropic");
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.anthropic.com/v1")
        );
        assert_eq!(provider.models.len(), 3);
        assert!(provider.models[0].default);
        assert!(provider.models[1..].iter().all(|m| !m.default));

        let auth = plugin.manifest.auth.expect("auth block");
        assert_eq!(auth.kind, AuthKind::ApiKey);
        assert!(auth.setting.is_none());
        assert!(auth.env.is_none());

        assert!(plugin.harness.is_none());
        assert!(plugin.gateway.is_none());
        assert!(plugin.connector.is_none());
    }

    #[test]
    fn ollama_no_auth_scheme_maps_to_auth_kind_none() {
        let plugin = provider_plugins()
            .into_iter()
            .find(|p| p.manifest.id == "ollama")
            .expect("ollama plugin");
        assert_eq!(plugin.manifest.auth.unwrap().kind, AuthKind::None);
    }

    #[test]
    fn oauth_and_free_categories_are_labeled() {
        let plugins = provider_plugins();
        let anthropic_oauth = plugins
            .iter()
            .find(|p| p.manifest.id == "anthropic-oauth")
            .unwrap();
        assert_eq!(
            anthropic_oauth.manifest.categories,
            vec!["model-provider".to_string(), "oauth".to_string()]
        );
        let kiro = plugins.iter().find(|p| p.manifest.id == "kiro").unwrap();
        assert_eq!(
            kiro.manifest.categories,
            vec!["model-provider".to_string(), "free".to_string()]
        );
    }

    #[tokio::test]
    async fn list_models_returns_seed_models_with_no_connection() {
        let store = mem_store().await;
        let models = list_models(&store, "anthropic").await.unwrap();
        assert_eq!(
            models,
            vec![
                "claude-opus-4-5".to_string(),
                "claude-sonnet-4-5".to_string(),
                "claude-haiku-4-5".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn list_models_applies_connection_override() {
        let store = mem_store().await;
        add_connection(
            &store,
            ConnectionRow {
                id: "c1".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "test".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    api_key: Some("sk-test".into()),
                    models_override: Some(vec!["custom-model".into()]),
                    ..Default::default()
                },
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let models = list_models(&store, "anthropic").await.unwrap();
        assert_eq!(models, vec!["custom-model".to_string()]);
    }

    #[tokio::test]
    async fn list_models_unknown_provider_id_returns_empty() {
        let store = mem_store().await;
        let models = list_models(&store, "nope").await.unwrap();
        assert!(models.is_empty());
    }

    #[tokio::test]
    async fn list_models_skips_disabled_connection_override() {
        let store = mem_store().await;
        add_connection(
            &store,
            ConnectionRow {
                id: "c1".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "disabled".into(),
                priority: 0,
                enabled: false,
                data: ConnectionData {
                    api_key: Some("sk-test".into()),
                    models_override: Some(vec!["custom-model".into()]),
                    ..Default::default()
                },
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let models = list_models(&store, "anthropic").await.unwrap();
        assert_eq!(
            models,
            vec![
                "claude-opus-4-5".to_string(),
                "claude-sonnet-4-5".to_string(),
                "claude-haiku-4-5".to_string(),
            ]
        );
    }
}
