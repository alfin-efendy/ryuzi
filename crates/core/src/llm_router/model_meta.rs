//! Per-model metadata (context window, max output, capabilities).
//!
//! Resolution order: settings override (`models.meta.<id>`, JSON object) →
//! refreshed models.dev snapshot on disk → vendored snapshot → FALLBACK.

use crate::llm_router::model_effort::{ExecutionSurfaceKey, ReasoningEffortOption};
use crate::store::Store;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Tokens held back from the usable window for the model's generated output,
/// so a prompt that fits the input budget doesn't overflow once the response
/// is appended. Capped so a large-output model doesn't over-reserve. Mirrors
/// opencode's overflow.ts (usable = context - reserved output).
const OUTPUT_RESERVE_CAP: u64 = 20_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
    /// USD per 1M tokens; 0.0 when the upstream omits a rate.
    #[serde(default)]
    pub cost_input: f64,
    #[serde(default)]
    pub cost_output: f64,
    #[serde(default)]
    pub cost_cache_read: f64,
    #[serde(default)]
    pub cost_cache_write: f64,
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
    cost_input: 0.0,
    cost_output: 0.0,
    cost_cache_read: 0.0,
    cost_cache_write: 0.0,
};

impl ModelMeta {
    /// Input token budget: the context window minus reserved output headroom,
    /// floored at half the window. See [`OUTPUT_RESERVE_CAP`].
    pub fn usable_window(&self) -> u64 {
        let reserved = self.max_output_tokens.min(OUTPUT_RESERVE_CAP);
        self.context_window
            .saturating_sub(reserved)
            .max(self.context_window / 2)
    }
    /// The auto-compact threshold at `percent` (settings default 90) — a
    /// fraction of the *usable* input budget, so proactive compaction fires
    /// below the hard ceiling with room for one more exchange.
    pub fn auto_compact_limit(&self, percent: u64) -> u64 {
        self.usable_window() * percent.min(95) / 100
    }
    /// USD for one request's four disjoint token buckets. Anthropic reports
    /// non-cached input, cache-read, and cache-creation separately, each at
    /// its own rate. Unknown rates (0.0) contribute 0.
    pub fn cost_usd(&self, input: u64, output: u64, cache_read: u64, cache_creation: u64) -> f64 {
        let per = |tokens: u64, rate: f64| (tokens as f64) / 1_000_000.0 * rate;
        per(input, self.cost_input)
            + per(output, self.cost_output)
            + per(cache_read, self.cost_cache_read)
            + per(cache_creation, self.cost_cache_write)
    }
}

static VENDORED: &str = include_str!("model_meta_snapshot.json");

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
struct CatalogModelMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context_window: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supports_prompt_cache: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    supports_reasoning: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    display_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    reasoning_efforts: Option<Vec<ReasoningEffortOption>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    default_reasoning_effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost_input: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost_output: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost_cache_read: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    cost_cache_write: Option<f64>,
}

impl CatalogModelMeta {
    fn merge_over(&self, mut base: ModelMeta, empty_effort_is_advertised: bool) -> ModelMeta {
        if let Some(value) = self.context_window {
            base.context_window = value;
        }
        if let Some(value) = self.max_output_tokens {
            base.max_output_tokens = value;
        }
        if let Some(value) = self.supports_prompt_cache {
            base.supports_prompt_cache = value;
        }
        if let Some(value) = self.supports_reasoning {
            base.supports_reasoning = value;
        }
        if let Some(value) = &self.display_name {
            base.display_name = Some(value.clone());
        }
        if let Some(options) = &self.reasoning_efforts {
            if empty_effort_is_advertised || !options.is_empty() {
                base.reasoning_efforts = options.clone();
            }
        }
        if let Some(value) = &self.default_reasoning_effort {
            base.default_reasoning_effort = Some(value.clone());
        }
        if let Some(value) = self.cost_input {
            base.cost_input = value;
        }
        if let Some(value) = self.cost_output {
            base.cost_output = value;
        }
        if let Some(value) = self.cost_cache_read {
            base.cost_cache_read = value;
        }
        if let Some(value) = self.cost_cache_write {
            base.cost_cache_write = value;
        }
        base
    }
}

const EXACT_PREFIX: &str = "provider::";
const EXACT_SEPARATOR: &str = "::model::";
const GENERIC_PREFIX: &str = "generic::";

fn exact_catalog_key(provider: &str, model: &str) -> String {
    format!("{EXACT_PREFIX}{provider}{EXACT_SEPARATOR}{model}")
}

fn generic_catalog_key(model: &str) -> String {
    format!("{GENERIC_PREFIX}{model}")
}

fn vendored() -> &'static HashMap<String, CatalogModelMeta> {
    static CACHE: std::sync::OnceLock<HashMap<String, CatalogModelMeta>> =
        std::sync::OnceLock::new();
    CACHE.get_or_init(|| serde_json::from_str(VENDORED).unwrap_or_default())
}

/// Lowercase, strip a `provider/` prefix, a `-latest` suffix, and trailing
/// date stamps (`-20250929` / `-2025-09-29`) so dated ids hit base entries.
#[cfg(test)]
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

fn normalize_model_id(id: &str) -> String {
    let mut s = id.to_ascii_lowercase();
    if let Some(base) = s.strip_suffix("-latest") {
        s = base.to_string();
    }
    let (base, tail) = s.rsplit_once('-').unwrap_or((&s, ""));
    if tail.len() == 8 && tail.bytes().all(|b| b.is_ascii_digit()) {
        return base.to_string();
    }
    let parts: Vec<&str> = s.rsplitn(4, '-').collect();
    if parts.len() == 4
        && parts[0].len() == 2
        && parts[1].len() == 2
        && parts[2].len() == 4
        && parts[..3]
            .iter()
            .all(|part| part.bytes().all(|b| b.is_ascii_digit()))
    {
        return parts[3].to_string();
    }
    s
}

/// Ties among normalized-key matches resolve to the lexicographically
/// smallest key, so the result is deterministic regardless of `HashMap`
/// iteration order.
fn lookup_generic_catalog(
    map: &HashMap<String, CatalogModelMeta>,
    id: &str,
) -> Option<CatalogModelMeta> {
    if let Some(meta) = map.get(&generic_catalog_key(id)) {
        return Some(meta.clone());
    }
    // Backward compatibility for pre-namespace generic snapshots. Direct
    // lookup is safe even when the model id itself contains slashes.
    if let Some(meta) = map.get(id) {
        return Some(meta.clone());
    }
    let normalized = normalize_model_id(id);
    map.iter()
        .filter_map(|(key, meta)| {
            let model = key.strip_prefix(GENERIC_PREFIX).or_else(|| {
                (!key.contains('/') && !key.starts_with(EXACT_PREFIX)).then_some(key.as_str())
            })?;
            (normalize_model_id(model) == normalized).then_some((key, meta))
        })
        .min_by(|a, b| a.0.cmp(b.0))
        .map(|(_, meta)| meta.clone())
}

fn lookup_exact_catalog(
    map: &HashMap<String, CatalogModelMeta>,
    provider: &str,
    model: &str,
) -> Option<CatalogModelMeta> {
    map.get(&exact_catalog_key(provider, model))
        .or_else(|| map.get(&format!("{provider}/{model}")))
        .cloned()
}

fn finalize_default(mut meta: ModelMeta) -> ModelMeta {
    if meta
        .default_reasoning_effort
        .as_ref()
        .is_some_and(|default| !meta.reasoning_efforts.iter().any(|o| &o.value == default))
    {
        meta.default_reasoning_effort = None;
    }
    meta
}

fn resolve_catalog_meta(
    surface: &ExecutionSurfaceKey,
    refreshed: Option<&HashMap<String, CatalogModelMeta>>,
    vendored: &HashMap<String, CatalogModelMeta>,
) -> ModelMeta {
    let mut resolved = lookup_generic_catalog(vendored, &surface.model).map_or_else(
        || FALLBACK.clone(),
        |meta| meta.merge_over(FALLBACK.clone(), true),
    );
    if let Some(exact) = lookup_exact_catalog(vendored, &surface.provider_id, &surface.model) {
        resolved = exact.merge_over(resolved, true);
    }
    if let Some(exact) =
        refreshed.and_then(|map| lookup_exact_catalog(map, &surface.provider_id, &surface.model))
    {
        // The models.dev refresh used to serialize an omitted effort field as
        // `[]`; treat that legacy cache shape as absent. Live connection
        // metadata retains authoritative `Some(vec![])` semantics separately.
        resolved = exact.merge_over(resolved, false);
    }
    finalize_default(resolved)
}

pub(crate) fn resolve_catalog_for_surface(surface: &ExecutionSurfaceKey) -> ModelMeta {
    resolve_catalog_meta(surface, refreshed().as_ref(), vendored())
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
        display_name: base.display_name,
        reasoning_efforts: base.reasoning_efforts,
        default_reasoning_effort: base.default_reasoning_effort,
        cost_input: v["cost_input"].as_f64().unwrap_or(base.cost_input),
        cost_output: v["cost_output"].as_f64().unwrap_or(base.cost_output),
        cost_cache_read: v["cost_cache_read"]
            .as_f64()
            .unwrap_or(base.cost_cache_read),
        cost_cache_write: v["cost_cache_write"]
            .as_f64()
            .unwrap_or(base.cost_cache_write),
    }
}

/// `~/.config/ryuzi/models-meta.json` — the refreshed snapshot location
/// (matches `memory::at_default`'s config-dir convention).
fn refreshed_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".config/ryuzi/models-meta.json"))
}

fn refreshed() -> Option<HashMap<String, CatalogModelMeta>> {
    let text = std::fs::read_to_string(refreshed_path()?).ok()?;
    serde_json::from_str(&text).ok()
}

/// Prune a full models.dev api.json into our snapshot map (same logic as
/// scripts/models-meta/update.ts).
fn prune_models_dev(api: &serde_json::Value) -> HashMap<String, CatalogModelMeta> {
    let mut out: HashMap<String, CatalogModelMeta> = HashMap::new();
    let Some(providers) = api.as_object() else {
        return out;
    };
    for (provider_id, provider) in providers {
        let Some(models) = provider.get("models").and_then(|m| m.as_object()) else {
            continue;
        };
        for (id, m) in models {
            let meta = CatalogModelMeta {
                context_window: Some(m["limit"]["context"].as_u64().unwrap_or(128_000)),
                max_output_tokens: Some(m["limit"]["output"].as_u64().unwrap_or(8_192)),
                supports_prompt_cache: Some(!m["cost"]["cache_read"].is_null()),
                supports_reasoning: Some(m["reasoning"].as_bool().unwrap_or(false)),
                display_name: m["name"].as_str().map(str::to_string),
                reasoning_efforts: None,
                default_reasoning_effort: None,
                cost_input: Some(m["cost"]["input"].as_f64().unwrap_or(0.0)),
                cost_output: Some(m["cost"]["output"].as_f64().unwrap_or(0.0)),
                cost_cache_read: Some(m["cost"]["cache_read"].as_f64().unwrap_or(0.0)),
                cost_cache_write: Some(m["cost"]["cache_write"].as_f64().unwrap_or(0.0)),
            };
            out.insert(exact_catalog_key(provider_id, id), meta.clone());
            let generic_key = generic_catalog_key(id);
            match out.get(&generic_key) {
                Some(prev)
                    if prev.context_window.unwrap_or_default()
                        >= meta.context_window.unwrap_or_default() => {}
                _ => {
                    out.insert(generic_key, meta);
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
            let lookup_requested = |map: &HashMap<String, CatalogModelMeta>| {
                lookup_generic_catalog(map, c).or_else(|| {
                    c.split_once('/')
                        .and_then(|(_, model)| lookup_generic_catalog(map, model))
                })
            };
            let vendored_meta = lookup_requested(vendored());
            let refreshed_meta = refreshed_map.as_ref().and_then(lookup_requested);
            (vendored_meta.is_some() || refreshed_meta.is_some()).then(|| {
                let base = vendored_meta.map_or_else(
                    || FALLBACK.clone(),
                    |meta| meta.merge_over(FALLBACK.clone(), true),
                );
                finalize_default(
                    refreshed_meta.map_or(base.clone(), |meta| meta.merge_over(base, false)),
                )
            })
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

pub(crate) async fn discovered_for_surface(
    store: &Store,
    surface: &ExecutionSurfaceKey,
) -> Option<crate::llm_router::model_effort::DiscoveredModelMeta> {
    let connection_id = surface.connection_id.as_ref()?;
    let connection = crate::llm_router::connections::get_connection(store, connection_id)
        .await
        .ok()??;
    if connection.provider != surface.provider_id {
        return None;
    }
    connection
        .data
        .model_meta_overrides
        .as_ref()?
        .get(&surface.model)
        .cloned()
}

/// Resolve metadata for one exact provider/connection/model execution surface.
pub async fn resolve_for_surface(store: &Store, surface: &ExecutionSurfaceKey) -> ModelMeta {
    let mut resolved = resolve_catalog_for_surface(surface);
    if let Some(discovered) = discovered_for_surface(store, surface).await {
        resolved = discovered.merge_over(resolved);
    }
    if let Ok(Some(raw)) = store
        .get_setting_raw(&format!("models.meta.{}", surface.model))
        .await
    {
        if let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) {
            resolved = apply_override(resolved, &value);
        }
    }
    resolved
}

#[cfg(test)]
mod cost_tests {
    use super::*;

    fn priced() -> ModelMeta {
        ModelMeta {
            context_window: 200_000,
            max_output_tokens: 8_192,
            supports_prompt_cache: true,
            supports_reasoning: false,
            display_name: None,
            reasoning_efforts: vec![],
            default_reasoning_effort: None,
            cost_input: 3.0,        // $3 / 1M
            cost_output: 15.0,      // $15 / 1M
            cost_cache_read: 0.3,   // $0.30 / 1M
            cost_cache_write: 3.75, // $3.75 / 1M
        }
    }

    #[test]
    fn each_bucket_priced_at_its_own_rate() {
        let m = priced();
        // 1M input @ $3 + 1M output @ $15 + 1M cache_read @ $0.30 + 1M cache_write @ $3.75
        let got = m.cost_usd(1_000_000, 1_000_000, 1_000_000, 1_000_000);
        assert!((got - (3.0 + 15.0 + 0.3 + 3.75)).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn scales_linearly_and_units_are_per_million() {
        let m = priced();
        // 500k input only → half of $3.
        let got = m.cost_usd(500_000, 0, 0, 0);
        assert!((got - 1.5).abs() < 1e-9, "got {got}");
    }

    #[test]
    fn zero_rates_contribute_zero() {
        assert_eq!(
            FALLBACK.cost_usd(1_000_000, 1_000_000, 1_000_000, 1_000_000),
            0.0
        );
    }

    #[test]
    fn a_known_vendored_model_has_nonzero_rates() {
        // At least one claude-sonnet entry must be priced. Fold the rate check
        // into the predicate: HashMap iteration order is randomized per process,
        // so a bare `.find(name-only)` could surface a free/alias zero-priced
        // variant and flake.
        let priced = super::vendored().iter().any(|(k, m)| {
            k.contains("claude")
                && k.contains("sonnet")
                && m.cost_input.unwrap_or_default() > 0.0
                && m.cost_output.unwrap_or_default() > 0.0
        });
        assert!(
            priced,
            "expected at least one priced claude-sonnet entry in the vendored snapshot"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn catalog(json: serde_json::Value) -> HashMap<String, CatalogModelMeta> {
        serde_json::from_value(json).unwrap()
    }

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
        let meta = lookup_generic_catalog(map, "claude-sonnet-4-5-20250929")
            .expect("normalized lookup should hit claude-sonnet-4-5");
        assert!(meta.context_window.unwrap_or_default() >= 200_000);
        assert_eq!(meta.supports_prompt_cache, Some(true));
    }

    #[test]
    fn lookup_fallback_tie_break_is_deterministic() {
        let older = CatalogModelMeta {
            context_window: Some(100),
            ..Default::default()
        };
        let newer = CatalogModelMeta {
            context_window: Some(200),
            ..Default::default()
        };
        let mut map = HashMap::new();
        map.insert(generic_catalog_key("m-20240101"), older);
        map.insert(generic_catalog_key("m-20250101"), newer);
        for _ in 0..20 {
            let meta = lookup_generic_catalog(&map, "m").expect("normalized fallback match");
            assert_eq!(
                meta.context_window,
                Some(100),
                "must always resolve to the lexicographically smallest key (m-20240101)"
            );
        }
    }

    #[test]
    fn finalize_default_does_not_guess_from_a_single_effort_option() {
        let meta = finalize_default(ModelMeta {
            reasoning_efforts: vec![ReasoningEffortOption {
                value: "ultra".into(),
                label: "Ultra".into(),
                description: None,
            }],
            default_reasoning_effort: None,
            ..FALLBACK
        });

        assert_eq!(meta.default_reasoning_effort, None);
    }

    #[test]
    fn refreshed_exact_fills_missing_effort_from_vendored_exact() {
        let surface = ExecutionSurfaceKey {
            provider_id: "openai-oauth".into(),
            connection_id: None,
            model: "gpt-review-fix".into(),
        };
        let refreshed = catalog(serde_json::json!({
            exact_catalog_key("openai-oauth", "gpt-review-fix"): {
                "context_window": 999000,
                "display_name": "Fresh display",
                "reasoning_efforts": [],
                "default_reasoning_effort": null
            }
        }));
        let vendored = catalog(serde_json::json!({
            exact_catalog_key("openai-oauth", "gpt-review-fix"): {
                "context_window": 128000,
                "reasoning_efforts": [{"value":"ultra","label":"ultra","description":"Deep"}],
                "default_reasoning_effort": "ultra"
            }
        }));

        let resolved = resolve_catalog_meta(&surface, Some(&refreshed), &vendored);
        assert_eq!(resolved.context_window, 999_000);
        assert_eq!(resolved.display_name.as_deref(), Some("Fresh display"));
        assert_eq!(resolved.reasoning_efforts[0].value, "ultra");
        assert_eq!(resolved.default_reasoning_effort.as_deref(), Some("ultra"));
    }

    #[test]
    fn generic_fallback_never_scans_provider_qualified_entries() {
        let model = "org/shared-model-20250101";
        let vendored = catalog(serde_json::json!({
            exact_catalog_key("provider-a", "org/shared-model"): {
                "context_window": 111,
                "display_name": "Provider A",
                "reasoning_efforts": [{"value":"leaked","label":"leaked","description":null}]
            },
            exact_catalog_key("provider-b", "org/shared-model"): {
                "context_window": 222,
                "display_name": "Provider B"
            },
            generic_catalog_key("org/shared-model"): {
                "context_window": 333,
                "display_name": "Generic"
            }
        }));
        let surface = ExecutionSurfaceKey {
            provider_id: "provider-c".into(),
            connection_id: None,
            model: model.into(),
        };

        let resolved = resolve_catalog_meta(&surface, None, &vendored);
        assert_eq!(resolved.context_window, 333);
        assert_eq!(resolved.display_name.as_deref(), Some("Generic"));
        assert!(resolved.reasoning_efforts.is_empty());
    }

    #[test]
    fn fallback_and_derived_limits() {
        assert_eq!(FALLBACK.context_window, 128_000);
        assert_eq!(FALLBACK.max_output_tokens, 8_192);
        let m = ModelMeta {
            context_window: 100_000,
            ..FALLBACK
        };
        // 100k context, 8_192 max output: reserve 8_192 -> usable 91_808;
        // proactive threshold = 90% of usable.
        assert_eq!(m.usable_window(), 91_808);
        assert_eq!(m.auto_compact_limit(90), 82_627);
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
            ..FALLBACK.clone()
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
                r#"{"context_window":32000,"display_name":"Injected","reasoning_efforts":[{"value":"high","label":"High","description":null}],"default_reasoning_effort":"high"}"#,
            )
            .await
            .unwrap();
        let meta = resolve(&store, "no-such-model-xyz").await;
        assert_eq!(meta.context_window, 32_000);
        assert_eq!(meta.max_output_tokens, FALLBACK.max_output_tokens);
        assert_eq!(meta.display_name, None);
        assert!(meta.reasoning_efforts.is_empty());
        assert_eq!(meta.default_reasoning_effort, None);
    }
}
