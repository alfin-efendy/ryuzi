//! Per-model metadata (context window, max output, capabilities).
//!
//! Resolution order: settings override (`models.meta.<id>`, JSON object) →
//! refreshed models.dev snapshot on disk → vendored snapshot → FALLBACK.

use crate::llm_router::model_effort::{ExecutionSurfaceKey, ReasoningEffortOption};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelMeta {
    pub context_window: u64,
    pub max_output_tokens: u64,
    #[serde(default)]
    pub supports_prompt_cache: bool,
    #[serde(default)]
    pub supports_reasoning: bool,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub reasoning_efforts: Vec<ReasoningEffortOption>,
    #[serde(default)]
    pub default_reasoning_effort: Option<String>,
}

/// Conservative metadata for unknown models (spec §5).
pub const FALLBACK: ModelMeta = ModelMeta {
    context_window: 128_000,
    max_output_tokens: 8_192,
    supports_prompt_cache: false,
    supports_reasoning: false,
    display_name: None,
    reasoning_efforts: Vec::new(),
    default_reasoning_effort: None,
};

impl ModelMeta {
    /// 95% of the raw window — headroom for the response (spec §5).
    pub fn usable_window(&self) -> u64 {
        self.context_window * 95 / 100
    }
    /// The auto-compact threshold at `percent` (settings default 90).
    pub fn auto_compact_limit(&self, percent: u64) -> u64 {
        self.context_window * percent.min(95) / 100
    }
}

static VENDORED: &str = include_str!("model_meta_snapshot.json");

fn vendored() -> &'static HashMap<String, ModelMeta> {
    static CACHE: std::sync::OnceLock<HashMap<String, ModelMeta>> = std::sync::OnceLock::new();
    CACHE.get_or_init(|| serde_json::from_str(VENDORED).unwrap_or_default())
}

/// Lowercase, strip a `provider/` prefix, a `-latest` suffix, and trailing
/// date stamps (`-20250929` / `-2025-09-29`) so dated ids hit base entries.
fn normalize(id: &str) -> String {
    let mut s = id.rsplit('/').next().unwrap_or(id).to_ascii_lowercase();
    if let Some(base) = s.strip_suffix("-latest") {
        s = base.to_string();
    }
    let strip_date = |s: &str| -> Option<String> {
        let (base, tail) = s.rsplit_once('-')?;
        if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) {
            return Some(base.to_string());
        }
        // -YYYY-MM-DD: three dash-separated numeric tails.
        let parts: Vec<&str> = s.rsplitn(4, '-').collect();
        if parts.len() == 4
            && parts[0].len() == 2
            && parts[1].len() == 2
            && parts[2].len() == 4
            && parts[..3]
                .iter()
                .all(|p| p.bytes().all(|b| b.is_ascii_digit()))
        {
            return Some(parts[3].to_string());
        }
        None
    };
    if let Some(base) = strip_date(&s) {
        s = base;
    }
    s
}

/// Ties among normalized-key matches resolve to the lexicographically
/// smallest key, so the result is deterministic regardless of `HashMap`
/// iteration order.
fn lookup(map: &HashMap<String, ModelMeta>, id: &str) -> Option<ModelMeta> {
    if let Some(m) = map.get(id) {
        return Some(m.clone());
    }
    let norm = normalize(id);
    if let Some(m) = map.get(&norm) {
        return Some(m.clone());
    }
    // Normalized key match on both sides (snapshot ids may carry dates too).
    map.iter()
        .filter(|(k, _)| normalize(k) == norm)
        .min_by(|a, b| a.0.cmp(b.0))
        .map(|(_, m)| m.clone())
}

/// Merge a partial JSON override (any subset of ModelMeta's fields) over base.
fn apply_override(base: ModelMeta, v: &serde_json::Value) -> ModelMeta {
    ModelMeta {
        context_window: v["context_window"].as_u64().unwrap_or(base.context_window),
        max_output_tokens: v["max_output_tokens"]
            .as_u64()
            .unwrap_or(base.max_output_tokens),
        supports_prompt_cache: v["supports_prompt_cache"]
            .as_bool()
            .unwrap_or(base.supports_prompt_cache),
        supports_reasoning: v["supports_reasoning"]
            .as_bool()
            .unwrap_or(base.supports_reasoning),
        display_name: v
            .get("display_name")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or(base.display_name),
        reasoning_efforts: v
            .get("reasoning_efforts")
            .and_then(|value| serde_json::from_value(value.clone()).ok())
            .unwrap_or(base.reasoning_efforts),
        default_reasoning_effort: v
            .get("default_reasoning_effort")
            .and_then(|value| value.as_str())
            .map(str::to_string)
            .or(base.default_reasoning_effort),
    }
}

fn apply_surface_settings_override(base: ModelMeta, v: &serde_json::Value) -> ModelMeta {
    let display_name = base.display_name.clone();
    let reasoning_efforts = base.reasoning_efforts.clone();
    let default_reasoning_effort = base.default_reasoning_effort.clone();
    let mut merged = apply_override(base, v);
    merged.display_name = display_name;
    merged.reasoning_efforts = reasoning_efforts;
    merged.default_reasoning_effort = default_reasoning_effort;
    merged
}

/// `~/.config/ryuzi/models-meta.json` — the refreshed snapshot location
/// (matches `memory::at_default`'s config-dir convention).
fn refreshed_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/ryuzi/models-meta.json"))
}

fn refreshed() -> Option<HashMap<String, ModelMeta>> {
    let text = std::fs::read_to_string(refreshed_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Prune a full models.dev api.json into our snapshot map (same logic as
/// scripts/models-meta/update.ts).
fn prune_models_dev(api: &serde_json::Value) -> HashMap<String, ModelMeta> {
    let mut out: HashMap<String, ModelMeta> = HashMap::new();
    let Some(providers) = api.as_object() else {
        return out;
    };
    for (provider_id, provider) in providers {
        let Some(models) = provider.get("models").and_then(|m| m.as_object()) else {
            continue;
        };
        for (id, m) in models {
            let meta = ModelMeta {
                context_window: m["limit"]["context"].as_u64().unwrap_or(128_000),
                max_output_tokens: m["limit"]["output"].as_u64().unwrap_or(8_192),
                supports_prompt_cache: !m["cost"]["cache_read"].is_null(),
                supports_reasoning: m["reasoning"].as_bool().unwrap_or(false),
                display_name: m["name"].as_str().map(str::to_string),
                reasoning_efforts: Vec::new(),
                default_reasoning_effort: None,
            };
            out.insert(format!("{provider_id}/{id}"), meta.clone());
            match out.get(id) {
                Some(prev) if prev.context_window >= meta.context_window => {}
                _ => {
                    out.insert(id.clone(), meta);
                }
            }
        }
    }
    out
}

/// Best-effort background refresh: at most once per 24h, silent on failure.
pub fn spawn_refresh() {
    tokio::spawn(async {
        let Some(path) = refreshed_path() else { return };
        let fresh = std::fs::metadata(&path)
            .and_then(|m| m.modified())
            .ok()
            .and_then(|t| t.elapsed().ok())
            .map(|e| e < std::time::Duration::from_secs(86_400))
            .unwrap_or(false);
        if fresh {
            return;
        }
        let Ok(resp) = reqwest::get("https://models.dev/api.json").await else {
            return;
        };
        let Ok(api) = resp.json::<serde_json::Value>().await else {
            return;
        };
        let pruned = prune_models_dev(&api);
        if pruned.is_empty() {
            return;
        }
        let Ok(json) = serde_json::to_string_pretty(&pruned) else {
            return;
        };
        if let Some(parent) = path.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let tmp = path.with_extension("json.tmp");
        if std::fs::write(&tmp, json).is_ok() {
            let _ = std::fs::rename(&tmp, &path);
        }
    });
}

/// Resolve metadata for a session's model. `requested` may be a route name
/// (e.g. "fable") — the routed upstream model id is tried as well. Never
/// fails; unknown models get [`FALLBACK`].
pub async fn resolve(store: &Store, requested: &str) -> ModelMeta {
    let mut candidates: Vec<String> = vec![requested.to_string()];
    if let Ok(Some(target)) =
        crate::llm_router::client::route_model_for_anthropic_messages(store, requested).await
    {
        if !candidates.contains(&target.upstream_model) {
            candidates.push(target.upstream_model);
        }
    }
    let refreshed_map = refreshed();
    let base = candidates
        .iter()
        .find_map(|c| {
            refreshed_map
                .as_ref()
                .and_then(|m| lookup(m, c))
                .or_else(|| lookup(vendored(), c))
        })
        .unwrap_or_else(|| FALLBACK.clone());
    // Settings override (raw key, JSON object value) — checked per candidate.
    for c in &candidates {
        if let Ok(Some(raw)) = store.get_setting_raw(&format!("models.meta.{c}")).await {
            if let Ok(v) = serde_json::from_str::<serde_json::Value>(&raw) {
                return apply_override(base, &v);
            }
        }
    }
    base
}

/// Resolve metadata for one exact provider/connection/model execution surface.
pub async fn resolve_for_surface(store: &Store, surface: &ExecutionSurfaceKey) -> ModelMeta {
    let exact_key = format!("{}/{}", surface.provider_id, surface.model);
    let refreshed_map = refreshed();
    let base = refreshed_map
        .as_ref()
        .and_then(|map| map.get(&exact_key).cloned())
        .or_else(|| vendored().get(&exact_key).cloned())
        .or_else(|| lookup(vendored(), &surface.model))
        .unwrap_or_else(|| FALLBACK.clone());

    let mut resolved = base;
    if let Some(connection_id) = &surface.connection_id {
        if let Ok(Some(connection)) =
            crate::llm_router::connections::get_connection(store, connection_id).await
        {
            if connection.provider == surface.provider_id {
                if let Some(discovered) = connection
                    .data
                    .model_meta_overrides
                    .as_ref()
                    .and_then(|metadata| metadata.get(&surface.model))
                {
                    resolved = discovered.merge_over(resolved);
                }
            }
        }
    }
    if let Ok(Some(raw)) = store
        .get_setting_raw(&format!("models.meta.{}", surface.model))
        .await
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            resolved = apply_surface_settings_override(resolved, &value);
        }
    }
    resolved
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_provider_prefix_dates_and_latest() {
        assert_eq!(normalize("openai/gpt-5.2-codex"), "gpt-5.2-codex");
        assert_eq!(normalize("claude-sonnet-4-5-20250929"), "claude-sonnet-4-5");
        assert_eq!(normalize("Claude-Sonnet-4-5-latest"), "claude-sonnet-4-5");
        assert_eq!(normalize("gemini-2.5-pro"), "gemini-2.5-pro");
    }

    #[test]
    fn vendored_snapshot_parses_and_lookup_hits_normalized_ids() {
        let map = vendored();
        assert!(!map.is_empty());
        let meta = lookup(map, "anthropic/claude-sonnet-4-5-20250929")
            .expect("normalized lookup should hit claude-sonnet-4-5");
        assert!(meta.context_window >= 200_000);
        assert!(meta.supports_prompt_cache);
    }

    #[test]
    fn lookup_fallback_tie_break_is_deterministic() {
        let older = ModelMeta {
            context_window: 100,
            ..FALLBACK
        };
        let newer = ModelMeta {
            context_window: 200,
            ..FALLBACK
        };
        let mut map = HashMap::new();
        map.insert("m-20240101".to_string(), older);
        map.insert("m-20250101".to_string(), newer);
        for _ in 0..20 {
            let meta = lookup(&map, "m").expect("normalized fallback match");
            assert_eq!(
                meta.context_window, 100,
                "must always resolve to the lexicographically smallest key (m-20240101)"
            );
        }
    }

    #[test]
    fn fallback_and_derived_limits() {
        assert_eq!(FALLBACK.context_window, 128_000);
        assert_eq!(FALLBACK.max_output_tokens, 8_192);
        let m = ModelMeta {
            context_window: 100_000,
            ..FALLBACK
        };
        assert_eq!(m.usable_window(), 95_000);
        assert_eq!(m.auto_compact_limit(90), 90_000);
    }

    #[test]
    fn override_merges_partial_fields_over_base() {
        let base = ModelMeta {
            context_window: 200_000,
            max_output_tokens: 64_000,
            supports_prompt_cache: true,
            supports_reasoning: true,
            display_name: None,
            reasoning_efforts: vec![],
            default_reasoning_effort: None,
        };
        let merged = apply_override(base, &serde_json::json!({"context_window": 150000}));
        assert_eq!(merged.context_window, 150_000);
        assert_eq!(merged.max_output_tokens, 64_000);
        assert!(merged.supports_prompt_cache);
    }

    #[tokio::test]
    async fn resolve_prefers_settings_override_then_snapshot_then_fallback() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        // Unknown model → fallback.
        assert_eq!(resolve(&store, "no-such-model-xyz").await, FALLBACK);
        // Settings override wins (raw write bypasses the schema, matching
        // scheduler_*'s raw-key precedent; the settable path is Task 4).
        store
            .set_setting_raw(
                "models.meta.no-such-model-xyz",
                r#"{"context_window": 32000}"#,
            )
            .await
            .unwrap();
        let meta = resolve(&store, "no-such-model-xyz").await;
        assert_eq!(meta.context_window, 32_000);
        assert_eq!(meta.max_output_tokens, FALLBACK.max_output_tokens);
    }
}
