use crate::llm_router::model_meta::ModelMeta;
use crate::llm_router::{connections, model_capabilities, registry, routes};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelPreferenceKey {
    pub family: String,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionSurfaceKey {
    pub provider_id: String,
    pub connection_id: Option<String>,
    pub model: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningEffortOption {
    pub value: String,
    pub label: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionModelEffortCapabilities {
    pub surface: ExecutionSurfaceKey,
    pub model_display_name: String,
    pub supported: Vec<ReasoningEffortOption>,
    pub provider_default: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub enum SelectableModelKind {
    Concrete,
    NamedRoute,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub enum ModelDefaultSource {
    Configured,
    Provider,
    VariesByTarget,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub enum StoredEffortStatus {
    Valid,
    Unsupported,
    UnknownMetadata,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub enum EffectiveEffortSource {
    Project,
    Session,
    RouteCompatibility,
    Configured,
    Provider,
    None,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct SelectableModelInfo {
    pub kind: SelectableModelKind,
    pub request_value: String,
    pub display_name: String,
    pub preference_key: Option<ModelPreferenceKey>,
    pub supported: Vec<ReasoningEffortOption>,
    pub configured_default: Option<String>,
    pub resolved_default: Option<String>,
    pub default_source: ModelDefaultSource,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct EffectiveEffort {
    pub value: Option<String>,
    pub label: Option<String>,
    pub source: EffectiveEffortSource,
    pub stored_status: Option<StoredEffortStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct RouteTargetEffortKey {
    pub route_id: String,
    pub target_index: u32,
}

#[derive(Debug, Clone)]
pub struct TurnEffortPolicy {
    pub requested_model: String,
    pub project_override: Option<String>,
    pub route_compatibility: HashMap<RouteTargetEffortKey, String>,
    pub configured: HashMap<ModelPreferenceKey, String>,
    pub surfaces: HashMap<ExecutionSurfaceKey, ExecutionModelEffortCapabilities>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, specta::Type)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRuntimeInfo {
    pub project_id: String,
    pub model: Option<String>,
    pub stored_effort: Option<String>,
    pub effective_effort: Option<String>,
    pub effective_effort_label: Option<String>,
    pub effective_source: EffectiveEffortSource,
    pub stored_effort_status: StoredEffortStatus,
    pub model_info: Option<SelectableModelInfo>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredModelMeta {
    pub display_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub effort_options: Option<Vec<ReasoningEffortOption>>,
    pub default_effort_advertised: bool,
    pub default_effort: Option<String>,
}

impl DiscoveredModelMeta {
    pub(crate) fn merge_over(&self, mut base: ModelMeta) -> ModelMeta {
        if let Some(display_name) = &self.display_name {
            base.display_name = Some(display_name.clone());
        }
        if let Some(options) = &self.effort_options {
            base.reasoning_efforts = options.clone();
        }
        if self.default_effort_advertised {
            base.default_reasoning_effort = self.default_effort.clone();
        }
        if base
            .default_reasoning_effort
            .as_ref()
            .is_some_and(|default| !base.reasoning_efforts.iter().any(|o| &o.value == default))
        {
            base.default_reasoning_effort = None;
        }
        if base.default_reasoning_effort.is_none() && base.reasoning_efforts.len() == 1 {
            base.default_reasoning_effort = Some(base.reasoning_efforts[0].value.clone());
        }
        base
    }
}

#[allow(dead_code)] // Consumed by structured model listing in the next plan task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EffortIntersection {
    pub supported: Vec<ReasoningEffortOption>,
    pub resolved_default: Option<String>,
    pub default_source: ModelDefaultSource,
}

#[allow(dead_code)] // Consumed by structured model listing in the next plan task.
pub(crate) fn intersect_capabilities(
    capabilities: &[ExecutionModelEffortCapabilities],
) -> EffortIntersection {
    let supported = capabilities.first().map_or_else(Vec::new, |first| {
        first
            .supported
            .iter()
            .filter(|option| {
                capabilities[1..].iter().all(|surface| {
                    surface
                        .supported
                        .iter()
                        .any(|other| other.value == option.value)
                })
            })
            .cloned()
            .collect()
    });

    let defaults: Vec<Option<String>> = capabilities
        .iter()
        .map(|surface| {
            surface
                .provider_default
                .as_ref()
                .filter(|default| surface.supported.iter().any(|o| &o.value == *default))
                .cloned()
                .or_else(|| {
                    (surface.supported.len() == 1).then(|| surface.supported[0].value.clone())
                })
        })
        .collect();
    let first_default = defaults.first().cloned().flatten();
    let uniform = defaults
        .first()
        .is_some_and(|first| defaults.iter().all(|default| default == first));
    let (resolved_default, default_source) =
        if defaults.is_empty() || (uniform && first_default.is_none()) {
            (None, ModelDefaultSource::None)
        } else if uniform {
            (first_default, ModelDefaultSource::Provider)
        } else {
            (None, ModelDefaultSource::VariesByTarget)
        };

    EffortIntersection {
        supported,
        resolved_default,
        default_source,
    }
}

fn resolved_surface_default(capabilities: &ExecutionModelEffortCapabilities) -> Option<String> {
    capabilities
        .provider_default
        .as_ref()
        .filter(|value| {
            capabilities
                .supported
                .iter()
                .any(|option| &option.value == *value)
        })
        .cloned()
        .or_else(|| {
            (capabilities.supported.len() == 1).then(|| capabilities.supported[0].value.clone())
        })
}

pub fn resolve_for_target(
    policy: &TurnEffortPolicy,
    route_target_key: Option<&RouteTargetEffortKey>,
    request_compatibility_effort: Option<&str>,
    preference_key: &ModelPreferenceKey,
    surface: &ExecutionSurfaceKey,
) -> EffectiveEffort {
    let Some(capabilities) = policy.surfaces.get(surface) else {
        return EffectiveEffort {
            value: None,
            label: None,
            source: EffectiveEffortSource::None,
            stored_status: Some(if policy.project_override.is_some() {
                StoredEffortStatus::UnknownMetadata
            } else {
                StoredEffortStatus::Valid
            }),
        };
    };
    let supported = |value: &str| {
        capabilities
            .supported
            .iter()
            .any(|option| option.value == value)
    };
    let stored_status = Some(match policy.project_override.as_deref() {
        Some(value) if !supported(value) => StoredEffortStatus::Unsupported,
        _ => StoredEffortStatus::Valid,
    });
    let route_value = route_target_key
        .and_then(|key| policy.route_compatibility.get(key).map(String::as_str))
        .or(request_compatibility_effort);
    let candidates = [
        (
            policy.project_override.as_deref(),
            EffectiveEffortSource::Project,
        ),
        (route_value, EffectiveEffortSource::RouteCompatibility),
        (
            policy.configured.get(preference_key).map(String::as_str),
            EffectiveEffortSource::Configured,
        ),
    ];
    let selected = candidates
        .into_iter()
        .find_map(|(value, source)| {
            value
                .filter(|value| supported(value))
                .map(|value| (value.to_string(), source))
        })
        .or_else(|| {
            resolved_surface_default(capabilities)
                .map(|value| (value, EffectiveEffortSource::Provider))
        });
    let (value, source) = selected
        .map_or((None, EffectiveEffortSource::None), |(value, source)| {
            (Some(value), source)
        });
    let label = value.as_ref().and_then(|value| {
        capabilities
            .supported
            .iter()
            .find(|option| &option.value == value)
            .map(|option| option.label.clone())
    });
    EffectiveEffort {
        value,
        label,
        source,
        stored_status,
    }
}

pub(crate) async fn capabilities_for_preference(
    store: &Store,
    key: &ModelPreferenceKey,
) -> anyhow::Result<Vec<ExecutionModelEffortCapabilities>> {
    let connections = connections::list_connections(store).await?;
    let mut capabilities = Vec::new();
    for connection in connections
        .into_iter()
        .filter(|connection| connection.enabled)
    {
        let Some(descriptor) = registry::descriptor(&connection.provider) else {
            continue;
        };
        if descriptor.family != key.family {
            continue;
        }
        let serves = connections::effective_models(descriptor, &connection)
            .iter()
            .any(|model| model == &key.model)
            || key.model.strip_suffix("-review").is_some_and(|base| {
                connections::effective_models(descriptor, &connection)
                    .iter()
                    .any(|model| model == base)
            });
        if !serves {
            continue;
        }
        let surface = ExecutionSurfaceKey {
            provider_id: connection.provider.clone(),
            connection_id: Some(connection.id.clone()),
            model: key.model.clone(),
        };
        capabilities
            .push(model_capabilities::resolve_for_surface(store, &key.family, &surface).await);
    }
    Ok(capabilities)
}

pub async fn set_preference(
    store: &Store,
    key: &ModelPreferenceKey,
    effort: Option<&str>,
) -> anyhow::Result<()> {
    let Some(effort) = effort else {
        return store.clear_model_effort_preference(key).await;
    };
    let capabilities = model_capabilities::resolve_for_model(store, key).await?;
    if !capabilities.supports(effort) {
        anyhow::bail!(
            "effort {effort:?} is not supported for {}/{}",
            key.family,
            key.model
        );
    }
    store.set_model_effort_preference(key, effort).await
}

async fn selection_capabilities(
    store: &Store,
    requested_model: &str,
) -> anyhow::Result<
    Option<(
        Vec<ModelPreferenceKey>,
        Vec<ExecutionModelEffortCapabilities>,
        HashMap<RouteTargetEffortKey, String>,
        bool,
    )>,
> {
    let route_list = routes::list_model_routes(store).await?;
    if let Some(route) = routes::route_by_name(&route_list, requested_model) {
        let mut keys = Vec::new();
        let mut surfaces = Vec::new();
        let mut compatibility = HashMap::new();
        for (index, target) in route.targets.iter().enumerate() {
            let key = ModelPreferenceKey {
                family: target.provider.clone(),
                model: target.model.clone(),
            };
            surfaces.extend(capabilities_for_preference(store, &key).await?);
            keys.push(key);
            if let Some(effort) = &target.effort {
                compatibility.insert(
                    RouteTargetEffortKey {
                        route_id: route.id.clone(),
                        target_index: index as u32,
                    },
                    effort.clone(),
                );
            }
        }
        return Ok(Some((keys, surfaces, compatibility, true)));
    }
    let Some((family, model)) = requested_model.split_once('/') else {
        return Ok(None);
    };
    let Some(family) = registry::family_of(family) else {
        return Ok(None);
    };
    let key = ModelPreferenceKey {
        family: family.to_string(),
        model: model.to_string(),
    };
    let capabilities = capabilities_for_preference(store, &key).await?;
    Ok(Some((vec![key], capabilities, HashMap::new(), false)))
}

pub async fn legacy_effort_supported_for_selection(
    store: &Store,
    requested_model: &str,
    effort: &str,
) -> anyhow::Result<bool> {
    let Some((keys, capabilities, _, is_named_route)) =
        selection_capabilities(store, requested_model).await?
    else {
        return Ok(false);
    };
    if is_named_route {
        for key in &keys {
            if store.get_model_effort_preference(key).await?.is_some() {
                return Ok(false);
            }
        }
    }
    Ok(intersect_capabilities(&capabilities)
        .supported
        .iter()
        .any(|option| option.value == effort))
}

async fn build_effort_policy(
    store: &Store,
    project_override: Option<String>,
    requested_model: &str,
) -> anyhow::Result<TurnEffortPolicy> {
    let configured = store
        .list_model_effort_preferences()
        .await?
        .into_iter()
        .collect();
    let (_, capabilities, route_compatibility, _) = selection_capabilities(store, requested_model)
        .await?
        .unwrap_or_default();
    let surfaces = capabilities
        .into_iter()
        .map(|capability| (capability.surface.clone(), capability))
        .collect();
    Ok(TurnEffortPolicy {
        requested_model: requested_model.to_string(),
        project_override,
        route_compatibility,
        configured,
        surfaces,
    })
}

pub async fn build_turn_effort_policy(
    store: &Store,
    project_id: &str,
    requested_model: &str,
) -> anyhow::Result<TurnEffortPolicy> {
    let project_override = store
        .get_project(project_id)
        .await?
        .and_then(|project| project.effort);
    build_effort_policy(store, project_override, requested_model).await
}

pub async fn build_session_effort_policy(
    store: &Store,
    session_pk: &str,
    requested_model: &str,
) -> anyhow::Result<TurnEffortPolicy> {
    let session_override = store
        .get_session_runtime_settings(session_pk)
        .await?
        .and_then(|runtime| runtime.effort);
    build_effort_policy(store, session_override, requested_model).await
}

pub async fn build_utility_effort_policy(
    store: &Store,
    requested_model: &str,
) -> anyhow::Result<TurnEffortPolicy> {
    build_effort_policy(store, None, requested_model).await
}

#[allow(dead_code)] // Consumed by tolerant routing in a later plan task.
pub(crate) fn parse_legacy_codex_selection(requested: &str) -> Option<(String, String)> {
    let (prefix, model) = requested
        .split_once('/')
        .map_or((None, requested), |(family, model)| (Some(family), model));
    if prefix.is_none() && !model.starts_with("gpt-") {
        return None;
    }
    const EFFORTS: [&str; 5] = ["none", "low", "medium", "high", "xhigh"];
    for effort in EFFORTS {
        let before_review = format!("-{effort}-review");
        let after_review = format!("-review-{effort}");
        let canonical_model = if let Some(base) = model.strip_suffix(&before_review) {
            format!("{base}-review")
        } else if let Some(base) = model.strip_suffix(&after_review) {
            format!("{base}-review")
        } else if let Some(base) = model.strip_suffix(&format!("-{effort}")) {
            base.to_string()
        } else {
            continue;
        };
        let canonical = prefix.map_or(canonical_model.clone(), |family| {
            format!("{family}/{canonical_model}")
        });
        return Some((canonical, effort.to_string()));
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::model_meta::ModelMeta;
    use crate::llm_router::models;
    use serde_json::json;

    fn option(value: &str, label: &str, description: Option<&str>) -> ReasoningEffortOption {
        ReasoningEffortOption {
            value: value.into(),
            label: label.into(),
            description: description.map(str::to_string),
        }
    }

    fn capabilities(
        provider: &str,
        supported: Vec<ReasoningEffortOption>,
        provider_default: Option<&str>,
    ) -> ExecutionModelEffortCapabilities {
        ExecutionModelEffortCapabilities {
            surface: ExecutionSurfaceKey {
                provider_id: provider.into(),
                connection_id: Some(format!("{provider}-connection")),
                model: "gpt-custom".into(),
            },
            model_display_name: "GPT Custom".into(),
            supported,
            provider_default: provider_default.map(str::to_string),
        }
    }

    #[test]
    fn custom_effort_preserves_provider_text_and_order() {
        let first = capabilities(
            "openai-oauth",
            vec![
                option("ultra", "Delegate Ultra", Some("Provider description")),
                option("low", "Low", None),
            ],
            Some("ultra"),
        );
        let result = intersect_capabilities(&[first]);
        assert_eq!(
            result.supported,
            vec![
                option("ultra", "Delegate Ultra", Some("Provider description")),
                option("low", "Low", None),
            ]
        );
    }

    #[test]
    fn tauri_contracts_serialize_with_required_camel_case_spellings() {
        let surface = serde_json::to_value(ExecutionSurfaceKey {
            provider_id: "provider".into(),
            connection_id: None,
            model: "model".into(),
        })
        .unwrap();
        assert!(surface.get("providerId").is_some());
        assert!(surface.get("provider_id").is_none());

        let selectable = serde_json::to_value(SelectableModelInfo {
            kind: SelectableModelKind::NamedRoute,
            request_value: "route".into(),
            display_name: "Route".into(),
            preference_key: None,
            supported: vec![],
            configured_default: None,
            resolved_default: None,
            default_source: ModelDefaultSource::VariesByTarget,
        })
        .unwrap();
        assert_eq!(selectable["kind"], "namedRoute");
        assert!(selectable.get("requestValue").is_some());
        assert!(selectable.get("request_value").is_none());
        assert_eq!(selectable["defaultSource"], "variesByTarget");

        assert_eq!(
            serde_json::to_value(StoredEffortStatus::UnknownMetadata).unwrap(),
            "unknownMetadata"
        );
    }

    #[test]
    fn raw_value_is_the_label_when_provider_has_no_friendly_label() {
        let parsed = models::parse_models(
            "openai-oauth",
            &json!({"models": [{
                "slug": "gpt-custom",
                "supported_reasoning_levels": [{
                    "effort": "x_provider_value",
                    "description": "Exact provider value"
                }]
            }]}),
        );
        assert_eq!(
            parsed.1["gpt-custom"].effort_options.as_ref().unwrap()[0],
            option(
                "x_provider_value",
                "x_provider_value",
                Some("Exact provider value")
            )
        );
    }

    #[test]
    fn intersection_keeps_first_surface_order_and_labels() {
        let first = capabilities(
            "first",
            vec![
                option("ultra", "First Ultra", None),
                option("low", "First Low", None),
            ],
            Some("low"),
        );
        let second = capabilities(
            "second",
            vec![
                option("low", "Second Low", None),
                option("ultra", "Second Ultra", None),
            ],
            Some("low"),
        );
        let third = capabilities("third", vec![option("ultra", "Third Ultra", None)], None);
        let result = intersect_capabilities(&[first, second, third]);
        assert_eq!(result.supported, vec![option("ultra", "First Ultra", None)]);
    }

    #[test]
    fn zero_and_one_option_models_are_valid_and_single_option_is_default() {
        let zero = intersect_capabilities(&[capabilities("zero", vec![], None)]);
        assert!(zero.supported.is_empty());
        assert_eq!(zero.resolved_default, None);
        assert_eq!(zero.default_source, ModelDefaultSource::None);

        let one = intersect_capabilities(&[capabilities(
            "one",
            vec![option("ultra", "ultra", None)],
            None,
        )]);
        assert_eq!(one.supported, vec![option("ultra", "ultra", None)]);
        assert_eq!(one.resolved_default.as_deref(), Some("ultra"));
        assert_eq!(one.default_source, ModelDefaultSource::Provider);
    }

    #[test]
    fn differing_or_mixed_surface_defaults_vary_by_target() {
        let differing = intersect_capabilities(&[
            capabilities(
                "a",
                vec![option("low", "low", None), option("high", "high", None)],
                Some("low"),
            ),
            capabilities(
                "b",
                vec![option("low", "low", None), option("high", "high", None)],
                Some("high"),
            ),
        ]);
        assert_eq!(differing.resolved_default, None);
        assert_eq!(differing.default_source, ModelDefaultSource::VariesByTarget);

        let inherited_none = intersect_capabilities(&[
            capabilities(
                "a",
                vec![option("none", "none", None), option("high", "high", None)],
                Some("none"),
            ),
            capabilities(
                "b",
                vec![option("none", "none", None), option("high", "high", None)],
                None,
            ),
        ]);
        assert_eq!(inherited_none.resolved_default, None);
        assert_eq!(
            inherited_none.default_source,
            ModelDefaultSource::VariesByTarget
        );
    }

    #[test]
    fn advertised_none_value_is_distinct_from_no_default() {
        let explicit = intersect_capabilities(&[capabilities(
            "a",
            vec![option("none", "none", None), option("high", "high", None)],
            Some("none"),
        )]);
        assert_eq!(explicit.resolved_default.as_deref(), Some("none"));
        assert_eq!(explicit.default_source, ModelDefaultSource::Provider);
    }

    #[test]
    fn legacy_codex_selection_preserves_review_and_nested_model_ids() {
        assert_eq!(
            parse_legacy_codex_selection("openai/gpt-5.5-high-review"),
            Some(("openai/gpt-5.5-review".into(), "high".into()))
        );
        assert_eq!(
            parse_legacy_codex_selection("openai/gpt-5.5-review-high"),
            Some(("openai/gpt-5.5-review".into(), "high".into()))
        );
        assert_eq!(parse_legacy_codex_selection("openai/gpt-5.5-review"), None);
        assert_eq!(
            parse_legacy_codex_selection("openai/org/model-high-review"),
            Some(("openai/org/model-review".into(), "high".into()))
        );
        assert_eq!(parse_legacy_codex_selection("fast-high"), None);
    }

    #[test]
    fn effective_resolution_audits_stale_values_and_falls_through() {
        let preference = ModelPreferenceKey {
            family: "openai".into(),
            model: "gpt-custom".into(),
        };
        let surface = ExecutionSurfaceKey {
            provider_id: "openai-oauth".into(),
            connection_id: Some("c1".into()),
            model: "gpt-custom".into(),
        };
        let capabilities = capabilities(
            "openai-oauth",
            vec![option("low", "Low", None)],
            Some("low"),
        );
        let mut policy = TurnEffortPolicy {
            requested_model: "openai/gpt-custom".into(),
            project_override: Some("stale".into()),
            route_compatibility: HashMap::new(),
            configured: HashMap::from([(preference.clone(), "also-stale".into())]),
            surfaces: HashMap::from([(surface.clone(), capabilities)]),
        };

        let result = resolve_for_target(&policy, None, None, &preference, &surface);
        assert_eq!(result.value.as_deref(), Some("low"));
        assert_eq!(result.source, EffectiveEffortSource::Provider);
        assert_eq!(result.stored_status, Some(StoredEffortStatus::Unsupported));

        policy.surfaces.clear();
        let result = resolve_for_target(&policy, None, None, &preference, &surface);
        assert_eq!(result.value, None);
        assert_eq!(
            result.stored_status,
            Some(StoredEffortStatus::UnknownMetadata)
        );

        policy.project_override = None;
        let result = resolve_for_target(&policy, None, None, &preference, &surface);
        assert_eq!(result.stored_status, Some(StoredEffortStatus::Valid));
    }

    #[test]
    fn parses_codex_metadata_and_copies_it_to_review_variant() {
        let (ids, meta) = models::parse_models(
            "openai-oauth",
            &json!({"models": [{
                "slug": "gpt-5.5",
                "display_name": "GPT 5.5",
                "default_reasoning_level": "ultra",
                "supported_reasoning_efforts": [
                    {"value": "low", "label": "Quick", "description": "Fast"},
                    {"effort": "ultra", "description": "Deep"}
                ]
            }]}),
        );
        assert_eq!(ids, vec!["gpt-5.5", "gpt-5.5-review"]);
        let base = &meta["gpt-5.5"];
        assert_eq!(base.display_name.as_deref(), Some("GPT 5.5"));
        assert_eq!(base.default_effort.as_deref(), Some("ultra"));
        assert!(base.default_effort_advertised);
        assert_eq!(base.effort_options.as_ref().unwrap()[1].label, "ultra");
        assert_eq!(meta["gpt-5.5-review"], *base);
    }

    #[test]
    fn id_only_discovery_does_not_invent_effort_metadata() {
        let (_, meta) = models::parse_models("openai", &json!({"data": [{"id": "gpt-id-only"}]}));
        assert_eq!(meta["gpt-id-only"].effort_options, None);
        assert!(!meta["gpt-id-only"].default_effort_advertised);
        let serialized = serde_json::to_value(&meta["gpt-id-only"]).unwrap();
        assert!(serialized.get("effortOptions").is_none());
    }

    #[test]
    fn partial_merge_distinguishes_missing_empty_and_invalid_default() {
        let fallback = ModelMeta {
            context_window: 100,
            max_output_tokens: 20,
            supports_prompt_cache: false,
            supports_reasoning: true,
            display_name: Some("Fallback".into()),
            reasoning_efforts: vec![option("low", "Low", None), option("high", "High", None)],
            default_reasoning_effort: Some("high".into()),
            ..crate::llm_router::model_meta::FALLBACK.clone()
        };
        let missing = DiscoveredModelMeta::default();
        assert_eq!(
            missing.merge_over(fallback.clone()).reasoning_efforts,
            fallback.reasoning_efforts
        );

        let empty = DiscoveredModelMeta {
            effort_options: Some(vec![]),
            ..Default::default()
        };
        assert!(empty
            .merge_over(fallback.clone())
            .reasoning_efforts
            .is_empty());

        let invalid = DiscoveredModelMeta {
            effort_options: Some(vec![option("ultra", "ultra", None)]),
            default_effort_advertised: true,
            default_effort: Some("invalid".into()),
            ..Default::default()
        };
        assert_eq!(
            invalid
                .merge_over(fallback)
                .default_reasoning_effort
                .as_deref(),
            Some("ultra")
        );
    }

    #[tokio::test]
    async fn refresh_persists_ids_and_metadata_on_the_exact_connection() {
        use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
        use axum::{routing::get, Json, Router};

        let app = Router::new().route(
            "/v1/models",
            get(|| async {
                Json(json!({"data": [{
                    "id": "live-model",
                    "display_name": "Live Model",
                    "supported_reasoning_efforts": [{"value": "ultra"}],
                    "default_reasoning_effort": "ultra"
                }]}))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let mut connection = ConnectionRow {
            id: "effort-connection".into(),
            provider: "openai".into(),
            auth_type: "api_key".into(),
            label: "Effort".into(),
            priority: 0,
            enabled: true,
            data: ConnectionData {
                api_key: Some("test".into()),
                base_url_override: Some(format!("http://127.0.0.1:{port}/v1")),
                ..Default::default()
            },
            created_at: 0,
            updated_at: 0,
        };
        connections::add_connection(&store, connection.clone())
            .await
            .unwrap();
        models::refresh_connection_models(&store, &reqwest::Client::new(), &mut connection)
            .await
            .unwrap();

        let stored = connections::get_connection(&store, "effort-connection")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(stored.data.models_override, Some(vec!["live-model".into()]));
        let discovered = &stored.data.model_meta_overrides.unwrap()["live-model"];
        assert_eq!(discovered.display_name.as_deref(), Some("Live Model"));
        assert_eq!(
            discovered.effort_options.as_ref().unwrap()[0].value,
            "ultra"
        );
    }

    #[tokio::test]
    async fn exact_connection_metadata_is_authoritative_for_surface_resolution() {
        use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        let mut metadata = std::collections::HashMap::new();
        metadata.insert(
            "gpt-surface".into(),
            DiscoveredModelMeta {
                display_name: Some("Surface GPT".into()),
                effort_options: Some(vec![]),
                default_effort_advertised: true,
                default_effort: None,
            },
        );
        connections::add_connection(
            &store,
            ConnectionRow {
                id: "surface-connection".into(),
                provider: "openai-oauth".into(),
                auth_type: "oauth".into(),
                label: "Surface".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    model_meta_overrides: Some(metadata),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        let surface = ExecutionSurfaceKey {
            provider_id: "openai-oauth".into(),
            connection_id: Some("surface-connection".into()),
            model: "gpt-surface".into(),
        };
        store
            .set_setting_raw(
                "models.meta.gpt-surface",
                r#"{"context_window":64000,"reasoning_efforts":[{"value":"high","label":"High","description":null}]}"#,
            )
            .await
            .unwrap();
        let resolved = crate::llm_router::model_meta::resolve_for_surface(&store, &surface).await;
        assert_eq!(resolved.display_name.as_deref(), Some("Surface GPT"));
        assert!(resolved.reasoning_efforts.is_empty());
        assert_eq!(resolved.context_window, 64_000);
    }

    #[tokio::test]
    async fn preference_validation_accepts_pinned_anthropic_and_rejects_unknown_values() {
        use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        connections::add_connection(
            &store,
            ConnectionRow {
                id: "anthropic-pref".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Anthropic".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    models_override: Some(vec!["claude-opus-4-6".into()]),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        let key = ModelPreferenceKey {
            family: "anthropic".into(),
            model: "claude-opus-4-6".into(),
        };

        set_preference(&store, &key, Some("max")).await.unwrap();
        assert_eq!(
            store
                .get_model_effort_preference(&key)
                .await
                .unwrap()
                .as_deref(),
            Some("max")
        );

        let error = set_preference(&store, &key, Some("xhigh"))
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("is not supported for anthropic/claude-opus-4-6"));
    }

    #[tokio::test]
    async fn session_effort_policy_uses_the_durable_chat_override() {
        use crate::domain::{PermMode, Session, SessionKind, SessionStatus};

        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        store
            .insert_session(Session {
                session_pk: "chat-effort".into(),
                project_id: None,
                agent_session_id: None,
                worktree_path: None,
                branch: None,
                title: None,
                status: SessionStatus::Idle,
                perm_mode: PermMode::Default,
                started_by: None,
                created_at: None,
                last_active: None,
                resume_attempts: 0,
                branch_owned: false,
                kind: SessionKind::Chat,
                speaker: None,
                agent: None,
                parent_session_pk: None,
            })
            .await
            .unwrap();
        store
            .update_session_runtime_settings(
                "chat-effort",
                Some("openai/gpt-5.5".into()),
                Some("high".into()),
            )
            .await
            .unwrap();

        let policy = build_session_effort_policy(&store, "chat-effort", "openai/gpt-5.5")
            .await
            .unwrap();
        assert_eq!(policy.project_override.as_deref(), Some("high"));
    }
}
