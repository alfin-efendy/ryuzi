use crate::llm_router::model_effort::{DiscoveredModelMeta, ReasoningEffortOption};
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
        .or_else(|| (supported.len() == 1).then(|| supported[0].value.clone()))
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
