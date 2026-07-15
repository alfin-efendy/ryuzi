use crate::llm_router::model_effort::{
    DiscoveredModelMeta, ExecutionModelEffortCapabilities, ExecutionSurfaceKey, ModelPreferenceKey,
    ReasoningEffortOption,
};
use crate::llm_router::{connections, model_meta, registry};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

static VENDORED: &str = include_str!("model_capability_catalog.json");

pub const ANTHROPIC_EFFORT_SOURCE_URL: &str =
    "https://platform.claude.com/docs/en/build-with-claude/effort";
pub const ANTHROPIC_EFFORT_REVIEWED_ON: &str = "2026-07-12";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub enum ModelCapabilitySource {
    Discovery,
    VendoredFallback,
    ExistingCatalog,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelEffortCapabilities {
    pub supported: Vec<ReasoningEffortOption>,
    pub provider_default: Option<String>,
    pub source: ModelCapabilitySource,
}

impl ModelEffortCapabilities {
    pub fn supports(&self, value: &str) -> bool {
        self.supported.iter().any(|option| option.value == value)
    }
}

#[derive(Debug, Deserialize)]
struct VendoredCapabilityCatalog {
    source_url: String,
    reviewed_on: String,
    anthropic: HashMap<String, Vec<String>>,
}

fn vendored() -> &'static VendoredCapabilityCatalog {
    static CATALOG: std::sync::OnceLock<VendoredCapabilityCatalog> = std::sync::OnceLock::new();
    CATALOG.get_or_init(|| {
        let catalog: VendoredCapabilityCatalog = serde_json::from_str(VENDORED)
            .expect("embedded model capability catalog must be valid");
        assert_eq!(catalog.source_url, ANTHROPIC_EFFORT_SOURCE_URL);
        assert_eq!(catalog.reviewed_on, ANTHROPIC_EFFORT_REVIEWED_ON);
        catalog
    })
}

fn strip_terminal_date(model: &str) -> &str {
    if let Some((base, tail)) = model.rsplit_once('-') {
        if tail.len() == 8 && tail.bytes().all(|byte| byte.is_ascii_digit()) {
            return base;
        }
    }
    let mut parts = model.rsplitn(4, '-');
    let day = parts.next();
    let month = parts.next();
    let year = parts.next();
    let base = parts.next();
    match (day, month, year, base) {
        (Some(day), Some(month), Some(year), Some(base))
            if day.len() == 2
                && month.len() == 2
                && year.len() == 4
                && [day, month, year]
                    .iter()
                    .all(|part| part.bytes().all(|byte| byte.is_ascii_digit())) =>
        {
            base
        }
        _ => model,
    }
}

fn option(value: &str) -> ReasoningEffortOption {
    let mut chars = value.chars();
    let label = chars
        .next()
        .map(|first| first.to_uppercase().collect::<String>() + chars.as_str())
        .unwrap_or_default();
    ReasoningEffortOption {
        value: value.to_string(),
        label,
        description: None,
    }
}

fn valid_default(default: Option<&str>, supported: &[ReasoningEffortOption]) -> Option<String> {
    default
        .filter(|value| supported.iter().any(|option| option.value == *value))
        .map(str::to_string)
}

pub fn resolve_effort_capabilities(
    family: &str,
    model: &str,
    discovered: Option<&DiscoveredModelMeta>,
    existing_supported: &[ReasoningEffortOption],
    existing_default: Option<&str>,
    wire_protocol_supports_effort: bool,
) -> ModelEffortCapabilities {
    if wire_protocol_supports_effort {
        if let Some(discovered) = discovered.filter(|metadata| metadata.effort_options.is_some()) {
            let supported = discovered.effort_options.clone().unwrap_or_default();
            let provider_default = valid_default(
                discovered
                    .default_effort_advertised
                    .then_some(discovered.default_effort.as_deref())
                    .flatten(),
                &supported,
            );
            return ModelEffortCapabilities {
                supported,
                provider_default,
                source: ModelCapabilitySource::Discovery,
            };
        }

        if family == "anthropic" {
            if let Some(values) = vendored().anthropic.get(strip_terminal_date(model)) {
                let supported = values.iter().map(|value| option(value)).collect();
                return ModelEffortCapabilities {
                    supported,
                    provider_default: Some("high".into()),
                    source: ModelCapabilitySource::VendoredFallback,
                };
            }
        }

        if !existing_supported.is_empty() {
            return ModelEffortCapabilities {
                supported: existing_supported.to_vec(),
                provider_default: valid_default(existing_default, existing_supported),
                source: ModelCapabilitySource::ExistingCatalog,
            };
        }
    }

    ModelEffortCapabilities {
        supported: Vec::new(),
        provider_default: None,
        source: ModelCapabilitySource::Unknown,
    }
}

pub async fn resolve_for_surface(
    store: &Store,
    family: &str,
    surface: &ExecutionSurfaceKey,
) -> ExecutionModelEffortCapabilities {
    let catalog = model_meta::resolve_catalog_for_surface(surface);
    let discovered = model_meta::discovered_for_surface(store, surface).await;
    let wire_supports = registry::descriptor(&surface.provider_id).is_some_and(|descriptor| {
        descriptor.id != "kiro"
            && descriptor.family == family
            && (family != "anthropic"
                || matches!(descriptor.format, registry::ApiFormat::Anthropic))
    });
    let capabilities = resolve_effort_capabilities(
        family,
        &surface.model,
        discovered.as_ref(),
        &catalog.reasoning_efforts,
        catalog.default_reasoning_effort.as_deref(),
        wire_supports,
    );
    ExecutionModelEffortCapabilities {
        surface: surface.clone(),
        model_display_name: discovered
            .as_ref()
            .and_then(|metadata| metadata.display_name.clone())
            .or(catalog.display_name)
            .unwrap_or_else(|| surface.model.clone()),
        supported: capabilities.supported,
        provider_default: capabilities.provider_default,
    }
}

fn connection_serves_model(
    descriptor: &registry::ProviderDescriptor,
    connection: &connections::ConnectionRow,
    key: &ModelPreferenceKey,
) -> bool {
    if descriptor.family != key.family {
        return false;
    }
    let models = connections::effective_models(descriptor, connection);
    models.iter().any(|model| model == &key.model)
        || (key.family == "openai"
            && key
                .model
                .strip_suffix("-review")
                .is_some_and(|base| models.iter().any(|model| model == base)))
}

pub async fn concrete_model_is_available(
    store: &Store,
    key: &ModelPreferenceKey,
) -> anyhow::Result<bool> {
    Ok(connections::list_connections(store)
        .await?
        .into_iter()
        .any(|connection| {
            connection.enabled
                && registry::descriptor(&connection.provider)
                    .is_some_and(|descriptor| connection_serves_model(descriptor, &connection, key))
        }))
}

pub async fn resolve_for_model(
    store: &Store,
    key: &ModelPreferenceKey,
) -> anyhow::Result<ModelEffortCapabilities> {
    let mut surfaces = Vec::new();
    for connection in connections::list_connections(store)
        .await?
        .into_iter()
        .filter(|connection| connection.enabled)
    {
        let Some(descriptor) = registry::descriptor(&connection.provider) else {
            continue;
        };
        if !connection_serves_model(descriptor, &connection, key) {
            continue;
        }
        let surface = ExecutionSurfaceKey {
            provider_id: connection.provider.clone(),
            connection_id: Some(connection.id.clone()),
            model: key.model.clone(),
        };
        surfaces.push(resolve_for_surface(store, &key.family, &surface).await);
    }

    if surfaces.is_empty() {
        let fallback_provider = registry::descriptor(&key.family)
            .filter(|descriptor| descriptor.family == key.family)
            .map(|descriptor| descriptor.id)
            .unwrap_or(key.family.as_str());
        let catalog = model_meta::resolve_catalog_for_surface(&ExecutionSurfaceKey {
            provider_id: fallback_provider.into(),
            connection_id: None,
            model: key.model.clone(),
        });
        return Ok(resolve_effort_capabilities(
            &key.family,
            &key.model,
            None,
            &catalog.reasoning_efforts,
            catalog.default_reasoning_effort.as_deref(),
            registry::descriptor(fallback_provider).is_some_and(|descriptor| {
                descriptor.family == key.family
                    && (key.family != "anthropic"
                        || matches!(descriptor.format, registry::ApiFormat::Anthropic))
            }),
        ));
    }

    let supported = surfaces[0]
        .supported
        .iter()
        .filter(|option| {
            surfaces[1..].iter().all(|surface| {
                surface
                    .supported
                    .iter()
                    .any(|other| other.value == option.value)
            })
        })
        .cloned()
        .collect::<Vec<_>>();
    let defaults = surfaces
        .iter()
        .map(|surface| {
            surface
                .provider_default
                .as_ref()
                .filter(|value| {
                    supported
                        .iter()
                        .any(|option| option.value.as_str() == value.as_str())
                })
                .cloned()
        })
        .collect::<Vec<_>>();
    let provider_default = defaults
        .first()
        .cloned()
        .flatten()
        .filter(|first| defaults.iter().all(|value| value.as_ref() == Some(first)));
    Ok(ModelEffortCapabilities {
        supported,
        provider_default,
        source: ModelCapabilitySource::Discovery,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn values(resolved: &ModelEffortCapabilities) -> Vec<&str> {
        resolved
            .supported
            .iter()
            .map(|option| option.value.as_str())
            .collect()
    }

    fn resolve(family: &str, model: &str) -> ModelEffortCapabilities {
        resolve_effort_capabilities(family, model, None, &[], None, true)
    }

    #[test]
    fn pinned_anthropic_matrix_is_exact() {
        let cases = [
            (
                "claude-fable-5",
                vec!["low", "medium", "high", "max", "xhigh"],
            ),
            (
                "claude-mythos-5",
                vec!["low", "medium", "high", "max", "xhigh"],
            ),
            (
                "claude-mythos-preview",
                vec!["low", "medium", "high", "max"],
            ),
            (
                "claude-opus-4-8",
                vec!["low", "medium", "high", "max", "xhigh"],
            ),
            (
                "claude-opus-4-7",
                vec!["low", "medium", "high", "max", "xhigh"],
            ),
            ("claude-opus-4-6", vec!["low", "medium", "high", "max"]),
            (
                "claude-sonnet-5",
                vec!["low", "medium", "high", "max", "xhigh"],
            ),
            ("claude-sonnet-4-6", vec!["low", "medium", "high", "max"]),
            ("claude-opus-4-5", vec!["low", "medium", "high"]),
        ];

        for (model, expected) in cases {
            let resolved = resolve("anthropic", model);
            assert_eq!(values(&resolved), expected, "{model}");
            assert_eq!(
                resolved.provider_default.as_deref(),
                Some("high"),
                "{model}"
            );
            assert_eq!(
                resolved.source,
                ModelCapabilitySource::VendoredFallback,
                "{model}"
            );
        }
    }

    #[test]
    fn only_terminal_documented_date_aliases_match() {
        for alias in ["claude-opus-4-7-20260712", "claude-opus-4-7-2026-07-12"] {
            assert_eq!(
                resolve("anthropic", alias).source,
                ModelCapabilitySource::VendoredFallback
            );
        }
        for non_alias in [
            "claude-opus-4-7-latest",
            "claude-opus-4-7-20260712-beta",
            "vendor/claude-opus-4-7-20260712",
            "Claude Opus 4.7",
        ] {
            assert_eq!(
                resolve("anthropic", non_alias).source,
                ModelCapabilitySource::Unknown,
                "{non_alias}"
            );
        }
    }

    #[test]
    fn explicit_discovery_without_advertised_default_does_not_guess_single_option() {
        let discovered = DiscoveredModelMeta {
            display_name: None,
            effort_options: Some(vec![option("high")]),
            default_effort_advertised: false,
            default_effort: None,
        };

        let resolved =
            resolve_effort_capabilities("openai", "gpt-single", Some(&discovered), &[], None, true);

        assert_eq!(values(&resolved), ["high"]);
        assert_eq!(resolved.provider_default, None);
    }

    #[test]
    fn explicit_discovery_is_authoritative_even_when_empty() {
        let empty = DiscoveredModelMeta {
            display_name: Some("Live Claude".into()),
            effort_options: Some(vec![]),
            default_effort_advertised: true,
            default_effort: None,
        };
        let resolved = resolve_effort_capabilities(
            "anthropic",
            "claude-opus-4-7",
            Some(&empty),
            &[],
            None,
            true,
        );
        assert!(resolved.supported.is_empty());
        assert_eq!(resolved.provider_default, None);
        assert_eq!(resolved.source, ModelCapabilitySource::Discovery);
    }

    #[test]
    fn missing_discovery_capability_uses_fallback_but_custom_values_win() {
        let missing = DiscoveredModelMeta {
            display_name: Some("Live Claude".into()),
            effort_options: None,
            default_effort_advertised: false,
            default_effort: None,
        };
        assert_eq!(
            resolve_effort_capabilities(
                "anthropic",
                "claude-opus-4-7",
                Some(&missing),
                &[],
                None,
                true,
            )
            .source,
            ModelCapabilitySource::VendoredFallback,
        );

        let custom = DiscoveredModelMeta {
            display_name: None,
            effort_options: Some(vec![ReasoningEffortOption {
                value: "focused".into(),
                label: "Focused".into(),
                description: Some("Provider supplied".into()),
            }]),
            default_effort_advertised: true,
            default_effort: Some("focused".into()),
        };
        let resolved = resolve_effort_capabilities(
            "anthropic",
            "claude-opus-4-7",
            Some(&custom),
            &[],
            None,
            true,
        );
        assert_eq!(values(&resolved), vec!["focused"]);
        assert_eq!(resolved.provider_default.as_deref(), Some("focused"));
        assert_eq!(resolved.source, ModelCapabilitySource::Discovery);
    }

    #[test]
    fn fallback_is_family_model_and_wire_protocol_scoped() {
        for (family, model, wire_supports) in [
            ("custom-anthropic", "claude-opus-4-7", true),
            ("openrouter", "claude-opus-4-7", true),
            ("anthropic", "claude-opus-4-7", false),
            ("anthropic", "claude-unknown-99", true),
        ] {
            let resolved =
                resolve_effort_capabilities(family, model, None, &[], None, wire_supports);
            assert!(resolved.supported.is_empty(), "{family}/{model}");
            assert_eq!(
                resolved.source,
                ModelCapabilitySource::Unknown,
                "{family}/{model}"
            );
        }
    }

    #[test]
    fn existing_non_anthropic_catalog_remains_available() {
        let existing = [ReasoningEffortOption {
            value: "ultra".into(),
            label: "Ultra".into(),
            description: None,
        }];
        let resolved = resolve_effort_capabilities(
            "openai",
            "gpt-catalog-model",
            None,
            &existing,
            Some("ultra"),
            true,
        );
        assert_eq!(values(&resolved), vec!["ultra"]);
        assert_eq!(resolved.provider_default.as_deref(), Some("ultra"));
        assert_eq!(resolved.source, ModelCapabilitySource::ExistingCatalog);
    }
}

#[cfg(test)]
mod store_tests {
    use super::*;
    use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
    use crate::llm_router::model_effort::{ExecutionSurfaceKey, ModelPreferenceKey};
    use std::collections::HashMap;

    async fn store() -> (tempfile::NamedTempFile, crate::store::Store) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        (tmp, store)
    }

    fn connection(
        id: &str,
        provider: &str,
        models: &[&str],
        metadata: Option<HashMap<String, DiscoveredModelMeta>>,
    ) -> ConnectionRow {
        ConnectionRow {
            id: id.into(),
            provider: provider.into(),
            auth_type: if provider.ends_with("-oauth") {
                "oauth".into()
            } else {
                "api_key".into()
            },
            label: id.into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                models_override: Some(models.iter().map(|model| (*model).to_string()).collect()),
                model_meta_overrides: metadata,
                ..Default::default()
            },
            created_at: 0,
            updated_at: 0,
        }
    }

    fn discovered(values: Option<&[&str]>, default: Option<&str>) -> DiscoveredModelMeta {
        DiscoveredModelMeta {
            display_name: None,
            effort_options: values.map(|values| {
                values
                    .iter()
                    .map(|value| ReasoningEffortOption {
                        value: (*value).into(),
                        label: (*value).into(),
                        description: None,
                    })
                    .collect()
            }),
            default_effort_advertised: default.is_some(),
            default_effort: default.map(str::to_string),
        }
    }

    #[tokio::test]
    async fn anthropic_api_key_and_oauth_surfaces_share_the_pinned_fallback() {
        let (_tmp, store) = store().await;
        for (provider, model) in [
            ("anthropic", "claude-sonnet-5"),
            ("anthropic-oauth", "claude-opus-4-7-20260712"),
        ] {
            let surface = ExecutionSurfaceKey {
                provider_id: provider.into(),
                connection_id: None,
                model: model.into(),
            };
            let resolved = resolve_for_surface(&store, "anthropic", &surface).await;
            assert_eq!(resolved.provider_default.as_deref(), Some("high"));
            assert!(resolved
                .supported
                .iter()
                .any(|option| option.value == "xhigh"));
        }
    }

    #[tokio::test]
    async fn kiro_surface_does_not_advertise_effort_from_metadata() {
        let (_tmp, store) = store().await;
        let model = "claude-sonnet-4.5";
        let mut metadata = HashMap::new();
        metadata.insert(model.into(), discovered(Some(&["high"]), Some("high")));
        connections::add_connection(
            &store,
            connection("kiro-live", "kiro", &[model], Some(metadata)),
        )
        .await
        .unwrap();

        let surface = ExecutionSurfaceKey {
            provider_id: "kiro".into(),
            connection_id: Some("kiro-live".into()),
            model: model.into(),
        };
        let resolved = resolve_for_surface(&store, "kiro", &surface).await;

        assert!(resolved.supported.is_empty());
        assert_eq!(resolved.provider_default, None);
    }

    #[tokio::test]
    async fn discovered_empty_beats_fallback_on_a_real_connection() {
        let (_tmp, store) = store().await;
        let model = "claude-opus-4-7";
        let mut metadata = HashMap::new();
        metadata.insert(model.into(), discovered(Some(&[]), None));
        connections::add_connection(
            &store,
            connection("anthropic-live", "anthropic", &[model], Some(metadata)),
        )
        .await
        .unwrap();

        let surface = ExecutionSurfaceKey {
            provider_id: "anthropic".into(),
            connection_id: Some("anthropic-live".into()),
            model: model.into(),
        };
        let resolved = resolve_for_surface(&store, "anthropic", &surface).await;
        assert!(resolved.supported.is_empty());
        assert_eq!(resolved.provider_default, None);
    }

    #[tokio::test]
    async fn concrete_model_resolution_intersects_serving_surfaces() {
        let (_tmp, store) = store().await;
        let model = "claude-opus-4-7";
        for (id, values) in [
            ("anthropic-a", &["low", "high", "xhigh"][..]),
            ("anthropic-b", &["low", "high"][..]),
        ] {
            let mut metadata = HashMap::new();
            metadata.insert(model.into(), discovered(Some(values), Some("high")));
            connections::add_connection(
                &store,
                connection(id, "anthropic", &[model], Some(metadata)),
            )
            .await
            .unwrap();
        }

        let resolved = resolve_for_model(
            &store,
            &ModelPreferenceKey {
                family: "anthropic".into(),
                model: model.into(),
            },
        )
        .await
        .unwrap();
        assert_eq!(
            resolved
                .supported
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "high"],
        );
        assert_eq!(resolved.provider_default.as_deref(), Some("high"));
        assert_eq!(resolved.source, ModelCapabilitySource::Discovery);
    }

    #[tokio::test]
    async fn openai_review_model_is_served_by_base_id_and_keeps_review_metadata() {
        let (_tmp, store) = store().await;
        let metadata = HashMap::from([(
            "gpt-5.5-review".into(),
            discovered(Some(&["high"]), Some("high")),
        )]);
        connections::add_connection(
            &store,
            connection("codex", "openai-oauth", &["gpt-5.5"], Some(metadata)),
        )
        .await
        .unwrap();
        let key = ModelPreferenceKey {
            family: "openai".into(),
            model: "gpt-5.5-review".into(),
        };
        assert!(concrete_model_is_available(&store, &key).await.unwrap());
        let resolved = resolve_for_model(&store, &key).await.unwrap();
        assert_eq!(
            resolved
                .supported
                .iter()
                .map(|o| o.value.as_str())
                .collect::<Vec<_>>(),
            vec!["high"]
        );
        assert_eq!(resolved.provider_default.as_deref(), Some("high"));
    }

    #[tokio::test]
    async fn capability_fallback_does_not_make_an_unserved_model_available() {
        let (_tmp, store) = store().await;
        let key = ModelPreferenceKey {
            family: "anthropic".into(),
            model: "claude-opus-4-8".into(),
        };
        assert!(!concrete_model_is_available(&store, &key).await.unwrap());
        assert!(!resolve_for_model(&store, &key)
            .await
            .unwrap()
            .supported
            .is_empty());
    }

    #[tokio::test]
    async fn no_connection_fallback_is_exact_and_unknown_stays_empty() {
        let (_tmp, store) = store().await;
        let known = resolve_for_model(
            &store,
            &ModelPreferenceKey {
                family: "anthropic".into(),
                model: "claude-opus-4-6".into(),
            },
        )
        .await
        .unwrap();
        assert!(known.supports("max"));
        assert!(!known.supports("xhigh"));

        let unknown = resolve_for_model(
            &store,
            &ModelPreferenceKey {
                family: "anthropic".into(),
                model: "claude-invented".into(),
            },
        )
        .await
        .unwrap();
        assert!(unknown.supported.is_empty());
        assert_eq!(unknown.source, ModelCapabilitySource::Unknown);
    }
}
