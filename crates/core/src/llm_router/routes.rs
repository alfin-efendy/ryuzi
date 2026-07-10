//! Named model routes ("combo" aliases): expose a short model id that maps to
//! an ordered list of provider connection/model targets.
use crate::store::Store;
use serde::{Deserialize, Serialize};
use specta::Type;

const SETTING_KEY: &str = "llm_model_routes";
const ROUND_ROBIN_KEY_PREFIX: &str = "llm_model_route_round_robin_cursor.";
const ACCOUNT_ROUTE_SETTING_KEY: &str = "llm_provider_account_routes";
const ACCOUNT_ROUND_ROBIN_KEY_PREFIX: &str = "llm_provider_account_round_robin_cursor.";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "kebab-case")]
pub enum ModelRouteStrategy {
    Fallback,
    RoundRobin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteTarget {
    /// A family id (registry family head), e.g. "anthropic" — NOT a
    /// connection id. The router expands this to every enabled account in
    /// the family serving `model`, at request time.
    pub provider: String,
    pub model: String,
    /// Compatibility-only storage for legacy Codex virtual model suffixes.
    /// New route writes cannot edit this value directly.
    #[serde(default)]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteInfo {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub strategy: ModelRouteStrategy,
    pub targets: Vec<ModelRouteTarget>,
    pub created_at: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IndexedModelRouteTarget {
    pub original_index: u32,
    pub target: ModelRouteTarget,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ProviderAccountRouteInfo {
    pub provider: String,
    pub strategy: ModelRouteStrategy,
}

pub async fn list_model_routes(store: &Store) -> anyhow::Result<Vec<ModelRouteInfo>> {
    let Some(raw) = store.get_setting(SETTING_KEY).await? else {
        return Ok(Vec::new());
    };
    let mut routes: Vec<ModelRouteInfo> = serde_json::from_str(&raw).unwrap_or_default();
    routes.sort_by_key(|r| (r.created_at, r.name.clone()));
    Ok(routes)
}

pub async fn list_provider_account_routes(
    store: &Store,
) -> anyhow::Result<Vec<ProviderAccountRouteInfo>> {
    let Some(raw) = store.get_setting(ACCOUNT_ROUTE_SETTING_KEY).await? else {
        return Ok(Vec::new());
    };
    let mut routes: Vec<ProviderAccountRouteInfo> = serde_json::from_str(&raw).unwrap_or_default();
    routes.sort_by_key(|r| r.provider.clone());
    Ok(routes)
}

pub async fn provider_account_route(
    store: &Store,
    provider: &str,
) -> anyhow::Result<ProviderAccountRouteInfo> {
    let provider = provider.trim();
    let routes = list_provider_account_routes(store).await?;
    Ok(routes
        .into_iter()
        .find(|route| route.provider == provider)
        .unwrap_or_else(|| ProviderAccountRouteInfo {
            provider: provider.to_string(),
            strategy: ModelRouteStrategy::Fallback,
        }))
}

pub async fn save_provider_account_route(
    store: &Store,
    provider: &str,
    strategy: ModelRouteStrategy,
) -> anyhow::Result<ProviderAccountRouteInfo> {
    let provider = provider.trim();
    if provider.is_empty() {
        anyhow::bail!("provider is required");
    }
    let next = ProviderAccountRouteInfo {
        provider: provider.to_string(),
        strategy,
    };
    let mut routes = list_provider_account_routes(store).await?;
    match routes.iter().position(|route| route.provider == provider) {
        Some(index) => routes[index] = next.clone(),
        None => routes.push(next.clone()),
    }
    routes.sort_by_key(|r| r.provider.clone());
    store
        .set_setting(ACCOUNT_ROUTE_SETTING_KEY, &serde_json::to_string(&routes)?)
        .await?;
    Ok(next)
}

pub async fn ordered_provider_connection_ids(
    store: &Store,
    provider: &str,
    scope: &str,
    ids: &[String],
) -> anyhow::Result<Vec<String>> {
    let route = provider_account_route(store, provider).await?;
    if route.strategy != ModelRouteStrategy::RoundRobin || ids.len() <= 1 {
        return Ok(ids.to_vec());
    }
    let key = format!("{ACCOUNT_ROUND_ROBIN_KEY_PREFIX}{provider}.{scope}");
    let start = store
        .get_setting(&key)
        .await?
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(0)
        % ids.len();
    let next = (start + 1) % ids.len();
    store.set_setting(&key, &next.to_string()).await?;

    Ok(ids[start..]
        .iter()
        .chain(ids[..start].iter())
        .cloned()
        .collect())
}

pub async fn peek_provider_connection_ids(
    store: &Store,
    provider: &str,
    scope: &str,
    ids: &[String],
) -> anyhow::Result<Vec<String>> {
    let route = provider_account_route(store, provider).await?;
    if route.strategy != ModelRouteStrategy::RoundRobin || ids.len() <= 1 {
        return Ok(ids.to_vec());
    }
    let key = format!("{ACCOUNT_ROUND_ROBIN_KEY_PREFIX}{provider}.{scope}");
    let start = store
        .get_setting(&key)
        .await?
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(0)
        % ids.len();
    Ok(ids[start..]
        .iter()
        .chain(ids[..start].iter())
        .cloned()
        .collect())
}

pub async fn save_model_route(
    store: &Store,
    route: ModelRouteInfo,
) -> anyhow::Result<ModelRouteInfo> {
    let routes = list_model_routes(store).await?;
    let prior = routes.iter().find(|stored| stored.id == route.id).cloned();
    let mut next = sanitize_route(route)?;
    let mut old_targets = prior.map(|route| route.targets).unwrap_or_default();
    let mut used = vec![false; old_targets.len()];
    for target in &mut next.targets {
        target.effort = old_targets
            .iter_mut()
            .enumerate()
            .find(|(index, old)| {
                !used[*index] && old.provider == target.provider && old.model == target.model
            })
            .and_then(|(index, old)| {
                used[index] = true;
                old.effort.take()
            });
    }
    let now = crate::paths::now_ms();
    if next.id.trim().is_empty() {
        next.id = crate::paths::new_id();
    }
    if next.created_at <= 0 {
        next.created_at = now;
    }
    next.updated_at = now;

    let mut routes = routes;
    if routes
        .iter()
        .any(|r| r.id != next.id && r.name.eq_ignore_ascii_case(&next.name))
    {
        anyhow::bail!("route name already exists: {}", next.name);
    }
    match routes.iter().position(|r| r.id == next.id) {
        Some(index) => routes[index] = next.clone(),
        None => routes.push(next.clone()),
    }
    persist_routes(store, &routes).await?;
    Ok(next)
}

pub async fn delete_model_route(store: &Store, id: &str) -> anyhow::Result<()> {
    let mut routes = list_model_routes(store).await?;
    routes.retain(|r| r.id != id);
    persist_routes(store, &routes).await
}

pub fn route_by_name<'a>(
    routes: &'a [ModelRouteInfo],
    requested: &str,
) -> Option<&'a ModelRouteInfo> {
    if requested.contains('/') {
        return None;
    }
    routes
        .iter()
        .find(|r| r.enabled && r.name == requested && !r.targets.is_empty())
}

pub async fn ordered_targets(
    store: &Store,
    route: &ModelRouteInfo,
) -> anyhow::Result<Vec<ModelRouteTarget>> {
    if route.strategy != ModelRouteStrategy::RoundRobin || route.targets.len() <= 1 {
        return Ok(route.targets.clone());
    }
    let key = format!("{ROUND_ROBIN_KEY_PREFIX}{}", route.id);
    let start = store
        .get_setting(&key)
        .await?
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(0)
        % route.targets.len();
    let next = (start + 1) % route.targets.len();
    store.set_setting(&key, &next.to_string()).await?;

    Ok(route.targets[start..]
        .iter()
        .chain(route.targets[..start].iter())
        .cloned()
        .collect())
}

pub async fn peek_ordered_targets(
    store: &Store,
    route: &ModelRouteInfo,
) -> anyhow::Result<Vec<ModelRouteTarget>> {
    if route.strategy != ModelRouteStrategy::RoundRobin || route.targets.len() <= 1 {
        return Ok(route.targets.clone());
    }
    let key = format!("{ROUND_ROBIN_KEY_PREFIX}{}", route.id);
    let start = store
        .get_setting(&key)
        .await?
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(0)
        % route.targets.len();
    Ok(route.targets[start..]
        .iter()
        .chain(route.targets[..start].iter())
        .cloned()
        .collect())
}

pub(crate) async fn ordered_indexed_targets(
    store: &Store,
    route: &ModelRouteInfo,
) -> anyhow::Result<Vec<IndexedModelRouteTarget>> {
    let indexed = route
        .targets
        .iter()
        .cloned()
        .enumerate()
        .map(|(index, target)| IndexedModelRouteTarget {
            original_index: index as u32,
            target,
        })
        .collect::<Vec<_>>();
    if route.strategy != ModelRouteStrategy::RoundRobin || indexed.len() <= 1 {
        return Ok(indexed);
    }
    let key = format!("{ROUND_ROBIN_KEY_PREFIX}{}", route.id);
    let start = store
        .get_setting(&key)
        .await?
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(0)
        % indexed.len();
    store
        .set_setting(&key, &((start + 1) % indexed.len()).to_string())
        .await?;
    Ok(indexed[start..]
        .iter()
        .chain(indexed[..start].iter())
        .cloned()
        .collect())
}

async fn persist_routes(store: &Store, routes: &[ModelRouteInfo]) -> anyhow::Result<()> {
    let mut ordered = routes.to_vec();
    ordered.sort_by_key(|r| (r.created_at, r.name.clone()));
    store
        .set_setting(SETTING_KEY, &serde_json::to_string(&ordered)?)
        .await
}

fn sanitize_route(mut route: ModelRouteInfo) -> anyhow::Result<ModelRouteInfo> {
    route.id = route.id.trim().to_string();
    route.name = route.name.trim().to_string();
    if route.name.is_empty() {
        anyhow::bail!("route name is required");
    }
    if route.name.contains('/') {
        anyhow::bail!("route name cannot contain /");
    }
    if !route
        .name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        anyhow::bail!("route name can only contain letters, numbers, -, _, and .");
    }
    route.targets = route
        .targets
        .into_iter()
        .map(|mut target| {
            target.provider = target.provider.trim().to_string();
            target.model = target.model.trim().to_string();
            target
        })
        .filter(|target| !target.provider.is_empty() && !target.model.is_empty())
        .collect();
    for target in &route.targets {
        if crate::llm_router::registry::family_of(&target.provider)
            != Some(target.provider.as_str())
        {
            anyhow::bail!("unknown provider family: {}", target.provider);
        }
    }
    if route.targets.is_empty() {
        anyhow::bail!("route needs at least one target model");
    }
    Ok(route)
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn mem_store() -> Store {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let (_, path) = tmp.keep().unwrap();
        Store::open(&path).await.unwrap()
    }

    fn route(name: &str) -> ModelRouteInfo {
        ModelRouteInfo {
            id: "r1".into(),
            name: name.into(),
            enabled: true,
            strategy: ModelRouteStrategy::Fallback,
            targets: vec![ModelRouteTarget {
                provider: "openai".into(),
                model: "m1".into(),
                effort: None,
            }],
            created_at: 1,
            updated_at: 1,
        }
    }

    #[tokio::test]
    async fn save_lists_and_replaces_routes() {
        let store = mem_store().await;
        let saved = save_model_route(&store, route("smart")).await.unwrap();
        assert_eq!(saved.name, "smart");
        assert_eq!(list_model_routes(&store).await.unwrap().len(), 1);

        let mut updated = saved;
        updated.targets[0].model = "m2".into();
        save_model_route(&store, updated).await.unwrap();
        let routes = list_model_routes(&store).await.unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].targets[0].model, "m2");
    }

    #[tokio::test]
    async fn save_preserves_only_matching_stored_compatibility_effort() {
        let store = mem_store().await;
        let raw = r#"[{"id":"r1","name":"smart","enabled":true,"strategy":"fallback","targets":[{"provider":"openai","model":"m1","effort":"high"},{"provider":"openai","model":"m1","effort":"low"}],"createdAt":1,"updatedAt":1}]"#;
        store.set_setting(SETTING_KEY, raw).await.unwrap();

        let mut incoming = route("smart");
        incoming.targets = vec![
            ModelRouteTarget {
                provider: "openai".into(),
                model: "m1".into(),
                effort: Some("ignored".into()),
            },
            ModelRouteTarget {
                provider: "openai".into(),
                model: "m2".into(),
                effort: Some("ignored".into()),
            },
            ModelRouteTarget {
                provider: "openai".into(),
                model: "m1".into(),
                effort: None,
            },
        ];
        let saved = save_model_route(&store, incoming).await.unwrap();
        assert_eq!(saved.targets[0].effort.as_deref(), Some("high"));
        assert_eq!(saved.targets[1].effort, None);
        assert_eq!(saved.targets[2].effort.as_deref(), Some("low"));
    }

    #[tokio::test]
    async fn invalid_names_are_rejected() {
        let store = mem_store().await;
        let err = save_model_route(&store, route("provider/model"))
            .await
            .unwrap_err();
        assert!(err.to_string().contains("cannot contain"));
    }

    #[tokio::test]
    async fn targets_must_reference_a_family_head() {
        let store = mem_store().await;
        let mut bad = route("smart");
        bad.targets[0].provider = "anthropic-oauth".into(); // member, not head
        assert!(save_model_route(&store, bad).await.is_err());
        let mut unknown = route("smart2");
        unknown.targets[0].provider = "nope".into();
        assert!(save_model_route(&store, unknown).await.is_err());
    }

    #[tokio::test]
    async fn provider_account_routes_default_to_fallback_and_persist() {
        let store = mem_store().await;
        assert_eq!(
            provider_account_route(&store, "openai")
                .await
                .unwrap()
                .strategy,
            ModelRouteStrategy::Fallback
        );

        let saved = save_provider_account_route(&store, "openai", ModelRouteStrategy::RoundRobin)
            .await
            .unwrap();
        assert_eq!(saved.strategy, ModelRouteStrategy::RoundRobin);
        assert_eq!(
            provider_account_route(&store, "openai")
                .await
                .unwrap()
                .strategy,
            ModelRouteStrategy::RoundRobin
        );
    }

    #[tokio::test]
    async fn provider_account_round_robin_rotates_connection_ids() {
        let store = mem_store().await;
        save_provider_account_route(&store, "openai", ModelRouteStrategy::RoundRobin)
            .await
            .unwrap();
        let ids = vec!["c1".to_string(), "c2".to_string()];

        let first = ordered_provider_connection_ids(&store, "openai", "gpt", &ids)
            .await
            .unwrap();
        let second = ordered_provider_connection_ids(&store, "openai", "gpt", &ids)
            .await
            .unwrap();

        assert_eq!(first, vec!["c1".to_string(), "c2".to_string()]);
        assert_eq!(second, vec!["c2".to_string(), "c1".to_string()]);
    }

    #[tokio::test]
    async fn peek_helpers_preserve_round_robin_cursors() {
        let store = mem_store().await;
        save_provider_account_route(&store, "openai", ModelRouteStrategy::RoundRobin)
            .await
            .unwrap();
        let ids = vec!["c1".to_string(), "c2".to_string()];
        let mut model_route = route("smart");
        model_route.strategy = ModelRouteStrategy::RoundRobin;
        model_route.targets.push(ModelRouteTarget {
            provider: "anthropic".into(),
            model: "m2".into(),
            effort: None,
        });
        let model_route = save_model_route(&store, model_route).await.unwrap();

        assert_eq!(
            peek_provider_connection_ids(&store, "openai", "gpt", &ids)
                .await
                .unwrap(),
            ids
        );
        assert_eq!(
            peek_provider_connection_ids(&store, "openai", "gpt", &ids)
                .await
                .unwrap(),
            ids
        );
        assert_eq!(
            peek_ordered_targets(&store, &model_route)
                .await
                .unwrap()
                .iter()
                .map(|target| target.model.as_str())
                .collect::<Vec<_>>(),
            vec!["m1", "m2"]
        );
        assert_eq!(
            ordered_provider_connection_ids(&store, "openai", "gpt", &ids)
                .await
                .unwrap(),
            ids
        );
        assert_eq!(
            ordered_targets(&store, &model_route)
                .await
                .unwrap()
                .iter()
                .map(|target| target.model.as_str())
                .collect::<Vec<_>>(),
            vec!["m1", "m2"]
        );
    }
}
