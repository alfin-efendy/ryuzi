//! Named model routes ("combo" aliases): expose a short model id that maps to
//! an ordered list of provider connection/model targets.
use crate::llm_router::model_capabilities;
use crate::llm_router::model_effort::{ModelPreferenceKey, ReasoningEffortOption};
use crate::llm_router::{connections, registry};
use crate::store::Store;
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::BTreeSet;

const SETTING_KEY: &str = "llm_model_routes";
const EFFORT_MIGRATION_KEY: &str = "llm_model_route_effort_migration_v1";
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
    /// Explicit effort policy; `None` uses the model default.
    #[serde(default)]
    pub effort: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteTargetCapability {
    pub provider: String,
    pub model: String,
    pub supported: Vec<ReasoningEffortOption>,
    pub provider_default: Option<String>,
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
    migrate_legacy_route_target_effort(store).await?;
    load_model_routes(store).await
}

async fn load_model_routes(store: &Store) -> anyhow::Result<Vec<ModelRouteInfo>> {
    let Some(raw) = store.get_setting(SETTING_KEY).await? else {
        return Ok(Vec::new());
    };
    let mut routes: Vec<ModelRouteInfo> = serde_json::from_str(&raw)?;
    routes.sort_by_key(|r| (r.created_at, r.name.clone()));
    Ok(routes)
}

pub(crate) fn parse_legacy_openai_route_suffix(model: &str) -> Option<(String, String)> {
    let model = model.strip_suffix("-review").unwrap_or(model);
    for effort in ["minimal", "medium", "xhigh", "ultra", "high", "low"] {
        if let Some(base) = model.strip_suffix(&format!("-{effort}")) {
            if !base.is_empty() {
                return Some((base.to_string(), effort.to_string()));
            }
        }
    }
    None
}

pub async fn migrate_legacy_route_target_effort(store: &Store) -> anyhow::Result<()> {
    if store.get_setting(EFFORT_MIGRATION_KEY).await?.as_deref() == Some("done") {
        return Ok(());
    }

    let mut inventory = BTreeSet::new();
    for descriptor in registry::CATALOG
        .iter()
        .filter(|descriptor| descriptor.family == "openai")
    {
        inventory.extend(descriptor.models.iter().map(|model| (*model).to_string()));
    }
    for connection in connections::list_connections(store).await? {
        let Some(descriptor) = registry::descriptor(&connection.provider) else {
            continue;
        };
        if descriptor.family == "openai" {
            inventory.extend(connections::effective_models(descriptor, &connection));
        }
    }

    let Some(raw_routes) = store.get_setting(SETTING_KEY).await? else {
        return write_effort_migration_marker(store).await;
    };
    let mut routes: Vec<ModelRouteInfo> = serde_json::from_str(&raw_routes)?;
    let mut transformed = false;
    for route in &mut routes {
        for target in &mut route.targets {
            if target.provider != "openai"
                || target.effort.is_some()
                || inventory.contains(&target.model)
            {
                continue;
            }
            let Some((base, effort)) = parse_legacy_openai_route_suffix(&target.model) else {
                continue;
            };
            if !inventory.contains(&base) {
                continue;
            }
            let resolved = model_capabilities::resolve_for_model(
                store,
                &ModelPreferenceKey {
                    family: "openai".into(),
                    model: base.clone(),
                },
            )
            .await?;
            if resolved.supports(&effort) {
                target.model = base;
                target.effort = Some(effort);
                transformed = true;
            }
        }
    }

    if !transformed {
        return write_effort_migration_marker(store).await;
    }

    store
        .with_conn(move |conn| {
            let transaction =
                conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            persist_routes(&transaction, &routes)?;
            transaction.execute(
                "INSERT INTO settings(key, value) VALUES (?1, 'done') \
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [EFFORT_MIGRATION_KEY],
            )?;
            transaction.commit()
        })
        .await
}

async fn write_effort_migration_marker(store: &Store) -> anyhow::Result<()> {
    store
        .with_conn(|conn| {
            conn.execute(
                "INSERT INTO settings(key, value) VALUES (?1, 'done') \
                 ON CONFLICT(key) DO UPDATE SET value=excluded.value",
                [EFFORT_MIGRATION_KEY],
            )
            .map(|_| ())
        })
        .await
}

pub async fn list_model_route_target_capabilities(
    store: &Store,
) -> anyhow::Result<Vec<ModelRouteTargetCapability>> {
    let mut models = BTreeSet::new();
    for descriptor in registry::CATALOG {
        for &model in descriptor.models {
            models.insert((descriptor.family.to_string(), model.to_string()));
        }
    }
    for connection in connections::list_connections(store).await? {
        let Some(descriptor) = registry::descriptor(&connection.provider) else {
            continue;
        };
        for model in connections::effective_models(descriptor, &connection) {
            models.insert((descriptor.family.to_string(), model));
        }
    }

    let mut capabilities = Vec::with_capacity(models.len());
    for (provider, model) in models {
        let resolved = model_capabilities::resolve_for_model(
            store,
            &ModelPreferenceKey {
                family: provider.clone(),
                model: model.clone(),
            },
        )
        .await?;
        capabilities.push(ModelRouteTargetCapability {
            provider,
            model,
            supported: resolved.supported,
            provider_default: resolved.provider_default,
        });
    }
    Ok(capabilities)
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
        .set_setting(
            crate::domain::WriteOrigin::User,
            ACCOUNT_ROUTE_SETTING_KEY,
            &serde_json::to_string(&routes)?,
        )
        .await?;
    Ok(next)
}

pub async fn ordered_provider_connection_ids(
    store: &Store,
    provider: &str,
    scope: &str,
    ids: &[String],
) -> anyhow::Result<Vec<String>> {
    Ok(
        ordered_provider_connection_ids_with_strategy(store, provider, scope, ids)
            .await?
            .0,
    )
}

pub(crate) async fn ordered_provider_connection_ids_with_strategy(
    store: &Store,
    provider: &str,
    scope: &str,
    ids: &[String],
) -> anyhow::Result<(Vec<String>, ModelRouteStrategy)> {
    let route = provider_account_route(store, provider).await?;
    if route.strategy != ModelRouteStrategy::RoundRobin || ids.len() <= 1 {
        return Ok((ids.to_vec(), route.strategy));
    }
    let key = format!("{ACCOUNT_ROUND_ROBIN_KEY_PREFIX}{provider}.{scope}");
    let start = store
        .get_setting(&key)
        .await?
        .and_then(|raw| raw.parse::<usize>().ok())
        .unwrap_or(0)
        % ids.len();
    let next = (start + 1) % ids.len();
    store
        .set_setting(crate::domain::WriteOrigin::User, &key, &next.to_string())
        .await?;

    Ok((
        ids[start..]
            .iter()
            .chain(ids[..start].iter())
            .cloned()
            .collect(),
        route.strategy,
    ))
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
    migrate_legacy_route_target_effort(store).await?;
    let route = sanitize_route(route)?;
    validate_route_target_efforts(store, &route).await?;
    store
        .with_conn(move |conn| {
            let transaction =
                conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let routes = load_routes(&transaction)?;
            let saved = save_model_route_locked(&transaction, routes, route)?;
            transaction.commit()?;
            Ok(saved)
        })
        .await
}

/// Saves `route` only when no existing route has the same name, ignoring ASCII
/// case. Returns `None` without writing when the name is already taken.
pub async fn save_model_route_if_name_absent(
    store: &Store,
    route: ModelRouteInfo,
) -> anyhow::Result<Option<ModelRouteInfo>> {
    let route = sanitize_route(route)?;
    if list_model_routes(store)
        .await?
        .iter()
        .any(|existing| existing.name.eq_ignore_ascii_case(&route.name))
    {
        return Ok(None);
    }
    validate_route_target_efforts(store, &route).await?;
    store
        .with_conn(move |conn| {
            let transaction =
                conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let routes = load_routes(&transaction)?;
            if routes
                .iter()
                .any(|existing| existing.name.eq_ignore_ascii_case(&route.name))
            {
                return Ok(None);
            }
            let saved = save_model_route_locked(&transaction, routes, route)?;
            transaction.commit()?;
            Ok(Some(saved))
        })
        .await
}

fn save_model_route_locked(
    conn: &rusqlite::Connection,
    routes: Vec<ModelRouteInfo>,
    route: ModelRouteInfo,
) -> rusqlite::Result<ModelRouteInfo> {
    let mut next = route;
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
        return Err(rusqlite::Error::ToSqlConversionFailure(
            anyhow::anyhow!("route name already exists: {}", next.name).into(),
        ));
    }
    match routes.iter().position(|r| r.id == next.id) {
        Some(index) => routes[index] = next.clone(),
        None => routes.push(next.clone()),
    }
    persist_routes(conn, &routes)?;
    Ok(next)
}

pub async fn delete_model_route(store: &Store, id: &str) -> anyhow::Result<()> {
    let id = id.to_owned();
    store
        .with_conn(move |conn| {
            let transaction =
                conn.transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)?;
            let mut routes = load_routes(&transaction)?;
            routes.retain(|route| route.id != id);
            persist_routes(&transaction, &routes)?;
            transaction.commit()
        })
        .await
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
        .find(|r| r.enabled && r.name.eq_ignore_ascii_case(requested) && !r.targets.is_empty())
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
    store
        .set_setting(crate::domain::WriteOrigin::User, &key, &next.to_string())
        .await?;

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
        .set_setting(
            crate::domain::WriteOrigin::User,
            &key,
            &((start + 1) % indexed.len()).to_string(),
        )
        .await?;
    Ok(indexed[start..]
        .iter()
        .chain(indexed[..start].iter())
        .cloned()
        .collect())
}

fn load_routes(conn: &rusqlite::Connection) -> rusqlite::Result<Vec<ModelRouteInfo>> {
    let raw = conn
        .query_row(
            "SELECT value FROM settings WHERE key=?1",
            [SETTING_KEY],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    let mut routes: Vec<ModelRouteInfo> = raw
        .as_deref()
        .and_then(|raw| serde_json::from_str(raw).ok())
        .unwrap_or_default();
    routes.sort_by_key(|route: &ModelRouteInfo| (route.created_at, route.name.clone()));
    Ok(routes)
}

fn persist_routes(conn: &rusqlite::Connection, routes: &[ModelRouteInfo]) -> rusqlite::Result<()> {
    let mut ordered = routes.to_vec();
    ordered.sort_by_key(|route| (route.created_at, route.name.clone()));
    let value = serde_json::to_string(&ordered)
        .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
    conn.execute(
        "INSERT INTO settings(key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        rusqlite::params![SETTING_KEY, value],
    )
    .map(|_| ())
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
            if let Some(effort) = &mut target.effort {
                *effort = effort.trim().to_string();
                if effort.is_empty() {
                    anyhow::bail!("route target effort cannot be empty; use Model default");
                }
            }
            Ok(target)
        })
        .collect::<anyhow::Result<Vec<_>>>()?
        .into_iter()
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

async fn validate_route_target_efforts(
    store: &Store,
    route: &ModelRouteInfo,
) -> anyhow::Result<()> {
    for target in &route.targets {
        let Some(effort) = &target.effort else {
            continue;
        };
        let capabilities = model_capabilities::resolve_for_model(
            store,
            &ModelPreferenceKey {
                family: target.provider.clone(),
                model: target.model.clone(),
            },
        )
        .await?;
        if !capabilities.supports(effort) {
            anyhow::bail!(
                "effort {effort:?} is not supported for route target {}/{}",
                target.provider,
                target.model
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    const EFFORT_MIGRATION_KEY: &str = "llm_model_route_effort_migration_v1";

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

    #[test]
    fn parses_one_terminal_legacy_openai_effort_suffix_after_review() {
        for (model, expected) in [
            ("gpt-5-review-minimal", ("gpt-5-review", "minimal")),
            ("gpt-custom-high-review", ("gpt-custom", "high")),
            ("gpt-test-xhigh", ("gpt-test", "xhigh")),
            ("gpt-test-ultra", ("gpt-test", "ultra")),
            ("gpt-test-high", ("gpt-test", "high")),
            ("gpt-test-low", ("gpt-test", "low")),
            ("gpt-test-high-review", ("gpt-test", "high")),
        ] {
            assert_eq!(
                parse_legacy_openai_route_suffix(model),
                Some((expected.0.into(), expected.1.into())),
                "{model}"
            );
        }
        assert_eq!(
            parse_legacy_openai_route_suffix("gpt-test-review-xhigh"),
            Some(("gpt-test-review".into(), "xhigh".into()))
        );
        assert_eq!(parse_legacy_openai_route_suffix("gpt-test-review"), None);
        assert_eq!(parse_legacy_openai_route_suffix("-high"), None);
        assert_eq!(parse_legacy_openai_route_suffix("gpt-test-max"), None);
        assert_eq!(
            parse_legacy_openai_route_suffix("gpt-test-low-high"),
            Some(("gpt-test-low".into(), "high".into()))
        );
    }

    #[tokio::test]
    async fn list_migrates_supported_openai_legacy_suffixes_and_preserves_everything_else() {
        let store = mem_store().await;
        connections::add_connection(
            &store,
            openai_connection(
                "custom",
                vec!["gpt-custom", "gpt-custom-high"],
                &["high", "ultra"],
            ),
        )
        .await
        .unwrap();
        let original = routes_with_targets(vec![
            target("openai", "gpt-custom-ultra", None),
            target("openai", "gpt-custom-high", None),
            target("openai", "gpt-custom-high-review", None),
            target("openai", "gpt-custom-ultra", Some("low")),
            target("openai", "gpt-missing-high", None),
            target("openai", "gpt-custom-low", None),
            target("anthropic", "gpt-custom-high", None),
            target("openai", "gpt-custom-high", None),
            target("openai", "gpt-custom-unknown", None),
        ]);
        store
            .set_setting(crate::domain::WriteOrigin::User, SETTING_KEY, &original)
            .await
            .unwrap();

        let routes = list_model_routes(&store).await.unwrap();
        let targets = &routes[0].targets;
        assert_eq!(targets[0], target("openai", "gpt-custom", Some("ultra")));
        assert_eq!(targets[1], target("openai", "gpt-custom-high", None));
        assert_eq!(targets[2], target("openai", "gpt-custom", Some("high")));
        assert_eq!(
            targets[3],
            target("openai", "gpt-custom-ultra", Some("low"))
        );
        assert_eq!(targets[4], target("openai", "gpt-missing-high", None));
        assert_eq!(targets[5], target("openai", "gpt-custom-low", None));
        assert_eq!(targets[6], target("anthropic", "gpt-custom-high", None));
        assert_eq!(targets[7], target("openai", "gpt-custom-high", None));
        assert_eq!(targets[8], target("openai", "gpt-custom-unknown", None));
        assert_eq!(
            store
                .get_setting(EFFORT_MIGRATION_KEY)
                .await
                .unwrap()
                .as_deref(),
            Some("done")
        );
    }

    #[tokio::test]
    async fn stale_effort_migration_marker_does_not_suppress_migration() {
        let store = mem_store().await;
        connections::add_connection(
            &store,
            openai_connection("custom", vec!["gpt-custom"], &["high"]),
        )
        .await
        .unwrap();
        store
            .set_setting(
                crate::domain::WriteOrigin::User,
                SETTING_KEY,
                &routes_with_targets(vec![target("openai", "gpt-custom-high", None)]),
            )
            .await
            .unwrap();
        store
            .set_setting(crate::domain::WriteOrigin::User, EFFORT_MIGRATION_KEY, "1")
            .await
            .unwrap();

        let routes = list_model_routes(&store).await.unwrap();

        assert_eq!(
            routes[0].targets,
            vec![target("openai", "gpt-custom", Some("high"))]
        );
        assert_eq!(
            store
                .get_setting(EFFORT_MIGRATION_KEY)
                .await
                .unwrap()
                .as_deref(),
            Some("done")
        );
    }

    #[tokio::test]
    async fn migration_uses_openai_family_connection_inventory() {
        let store = mem_store().await;
        connections::add_connection(
            &store,
            connection("codex", "openai-oauth", vec!["gpt-custom"], &["high"]),
        )
        .await
        .unwrap();
        store
            .set_setting(
                crate::domain::WriteOrigin::User,
                SETTING_KEY,
                &routes_with_targets(vec![target("openai", "gpt-custom-high", None)]),
            )
            .await
            .unwrap();

        let routes = list_model_routes(&store).await.unwrap();

        assert_eq!(
            routes[0].targets,
            vec![target("openai", "gpt-custom", Some("high"))]
        );
    }

    #[tokio::test]
    async fn migration_preserves_uneligible_routes_json_and_marks_completion() {
        let store = mem_store().await;
        let raw = r#" [ { "id":"legacy", "name":"legacy", "enabled":true, "strategy":"fallback", "targets":[{"provider":"anthropic","model":"claude-high"}], "createdAt":1, "updatedAt":1 } ] "#;
        store
            .set_setting(crate::domain::WriteOrigin::User, SETTING_KEY, raw)
            .await
            .unwrap();

        list_model_routes(&store).await.unwrap();

        assert_eq!(
            store.get_setting(SETTING_KEY).await.unwrap().as_deref(),
            Some(raw)
        );
        assert_eq!(
            store
                .get_setting(EFFORT_MIGRATION_KEY)
                .await
                .unwrap()
                .as_deref(),
            Some("done")
        );
    }

    #[tokio::test]
    async fn migration_does_not_create_absent_routes_setting() {
        let store = mem_store().await;

        list_model_routes(&store).await.unwrap();

        assert!(store.get_setting(SETTING_KEY).await.unwrap().is_none());
        assert_eq!(
            store
                .get_setting(EFFORT_MIGRATION_KEY)
                .await
                .unwrap()
                .as_deref(),
            Some("done")
        );
    }

    #[tokio::test]
    async fn migration_is_byte_stable_and_save_runs_it_before_writing() {
        let store = mem_store().await;
        connections::add_connection(
            &store,
            openai_connection("custom", vec!["gpt-custom"], &["high"]),
        )
        .await
        .unwrap();
        store
            .set_setting(
                crate::domain::WriteOrigin::User,
                SETTING_KEY,
                &routes_with_targets(vec![target("openai", "gpt-custom-high", None)]),
            )
            .await
            .unwrap();

        save_model_route(&store, route("new")).await.unwrap();
        let first = store.get_setting(SETTING_KEY).await.unwrap().unwrap();
        list_model_routes(&store).await.unwrap();
        let second = store.get_setting(SETTING_KEY).await.unwrap().unwrap();
        assert_eq!(first, second);
        assert_eq!(
            serde_json::from_str::<Vec<ModelRouteInfo>>(&second).unwrap()[0].targets[0],
            target("openai", "gpt-custom", Some("high"))
        );
    }

    #[tokio::test]
    async fn malformed_routes_fail_without_writing_migration_marker() {
        let store = mem_store().await;
        store
            .set_setting(crate::domain::WriteOrigin::User, SETTING_KEY, "not json")
            .await
            .unwrap();

        assert!(list_model_routes(&store).await.is_err());
        assert!(store
            .get_setting(EFFORT_MIGRATION_KEY)
            .await
            .unwrap()
            .is_none());
    }

    fn target(provider: &str, model: &str, effort: Option<&str>) -> ModelRouteTarget {
        ModelRouteTarget {
            provider: provider.into(),
            model: model.into(),
            effort: effort.map(str::to_string),
        }
    }

    fn routes_with_targets(targets: Vec<ModelRouteTarget>) -> String {
        serde_json::to_string(&vec![ModelRouteInfo {
            id: "legacy".into(),
            name: "legacy".into(),
            enabled: true,
            strategy: ModelRouteStrategy::Fallback,
            targets,
            created_at: 1,
            updated_at: 1,
        }])
        .unwrap()
    }

    fn openai_connection(
        id: &str,
        models: Vec<&str>,
        efforts: &[&str],
    ) -> connections::ConnectionRow {
        connection(id, "openai", models, efforts)
    }

    fn connection(
        id: &str,
        provider: &str,
        models: Vec<&str>,
        efforts: &[&str],
    ) -> connections::ConnectionRow {
        connections::ConnectionRow {
            id: id.into(),
            provider: provider.into(),
            auth_type: "api_key".into(),
            label: "OpenAI".into(),
            priority: 0,
            enabled: true,
            data: connections::ConnectionData {
                models_override: Some(models.into_iter().map(str::to_string).collect()),
                model_meta_overrides: Some(HashMap::from([
                    ("gpt-custom".into(), discovered_efforts(efforts)),
                    ("gpt-custom-review".into(), discovered_efforts(efforts)),
                ])),
                ..Default::default()
            },
            created_at: 0,
            updated_at: 0,
        }
    }

    fn discovered_efforts(
        efforts: &[&str],
    ) -> crate::llm_router::model_effort::DiscoveredModelMeta {
        crate::llm_router::model_effort::DiscoveredModelMeta {
            display_name: None,
            effort_options: Some(
                efforts
                    .iter()
                    .map(|value| ReasoningEffortOption {
                        value: (*value).into(),
                        label: (*value).into(),
                        description: None,
                    })
                    .collect(),
            ),
            default_effort_advertised: false,
            default_effort: None,
        }
    }

    #[tokio::test]
    async fn save_if_name_absent_preserves_case_insensitive_existing_route() {
        let store = mem_store().await;
        let existing = save_model_route(&store, route("Free")).await.unwrap();
        let inserted = save_model_route_if_name_absent(&store, route("free"))
            .await
            .unwrap();

        assert!(inserted.is_none());
        assert_eq!(list_model_routes(&store).await.unwrap(), vec![existing]);
    }

    #[tokio::test]
    async fn save_if_name_absent_ignores_invalid_effort_for_existing_route() {
        let store = mem_store().await;
        let existing = save_model_route(&store, route("Free")).await.unwrap();
        let mut duplicate = route("free");
        duplicate.targets[0] = ModelRouteTarget {
            provider: "anthropic".into(),
            model: "claude-opus-4-5".into(),
            effort: Some("max".into()),
        };

        let saved = save_model_route_if_name_absent(&store, duplicate)
            .await
            .unwrap();

        assert!(saved.is_none());
        assert_eq!(list_model_routes(&store).await.unwrap(), vec![existing]);
    }

    #[tokio::test]
    async fn save_if_name_absent_rejects_invalid_effort_for_new_route() {
        let store = mem_store().await;
        let mut route = route("free");
        route.targets[0] = ModelRouteTarget {
            provider: "anthropic".into(),
            model: "claude-opus-4-5".into(),
            effort: Some("max".into()),
        };

        assert_eq!(
            save_model_route_if_name_absent(&store, route)
                .await
                .unwrap_err()
                .to_string(),
            "effort \"max\" is not supported for route target anthropic/claude-opus-4-5"
        );
    }

    #[tokio::test]
    async fn save_lists_and_replaces_routes() {
        let store = mem_store().await;
        let saved = save_model_route(&store, route("free")).await.unwrap();
        assert_eq!(saved.name, "free");
        assert_eq!(list_model_routes(&store).await.unwrap().len(), 1);

        let mut updated = saved;
        updated.targets[0].model = "m2".into();
        save_model_route(&store, updated).await.unwrap();
        let routes = list_model_routes(&store).await.unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].targets[0].model, "m2");
    }

    #[tokio::test]
    async fn explicit_supported_target_effort_persists_and_model_default_resets_it() {
        let store = mem_store().await;
        let mut route = route("free");
        route.targets[0] = ModelRouteTarget {
            provider: "anthropic".into(),
            model: "claude-opus-4-7".into(),
            effort: Some(" max ".into()),
        };

        let saved = save_model_route(&store, route).await.unwrap();
        assert_eq!(saved.targets[0].effort.as_deref(), Some("max"));

        let mut reset = saved;
        reset.targets[0].effort = None;
        assert_eq!(
            save_model_route(&store, reset).await.unwrap().targets[0].effort,
            None
        );
        assert_eq!(
            list_model_routes(&store).await.unwrap()[0].targets[0].effort,
            None
        );
    }

    #[tokio::test]
    async fn route_target_effort_rejects_kiro_metadata_without_wire_support() {
        let store = mem_store().await;
        connections::add_connection(
            &store,
            connections::ConnectionRow {
                id: "kiro".into(),
                provider: "kiro".into(),
                auth_type: "oauth".into(),
                label: "Kiro".into(),
                priority: 0,
                enabled: true,
                data: connections::ConnectionData {
                    models_override: Some(vec!["claude-sonnet-4.5".into()]),
                    model_meta_overrides: Some(std::collections::HashMap::from([(
                        "claude-sonnet-4.5".into(),
                        crate::llm_router::model_effort::DiscoveredModelMeta {
                            display_name: None,
                            effort_options: Some(vec![ReasoningEffortOption {
                                value: "high".into(),
                                label: "High".into(),
                                description: None,
                            }]),
                            default_effort_advertised: true,
                            default_effort: Some("high".into()),
                        },
                    )])),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        let mut route = route("kiro");
        route.targets[0] = ModelRouteTarget {
            provider: "kiro".into(),
            model: "claude-sonnet-4.5".into(),
            effort: Some("high".into()),
        };

        assert!(save_model_route(&store, route).await.is_err());
    }

    #[tokio::test]
    async fn route_target_effort_rejects_unsupported_unknown_and_empty_values() {
        let store = mem_store().await;
        let mut unsupported = route("unsupported");
        unsupported.targets[0] = ModelRouteTarget {
            provider: "anthropic".into(),
            model: "claude-opus-4-5".into(),
            effort: Some("max".into()),
        };
        assert_eq!(
            save_model_route(&store, unsupported)
                .await
                .unwrap_err()
                .to_string(),
            "effort \"max\" is not supported for route target anthropic/claude-opus-4-5"
        );

        let mut unknown = route("unknown");
        unknown.targets[0] = ModelRouteTarget {
            provider: "anthropic".into(),
            model: "claude-invented".into(),
            effort: Some("high".into()),
        };
        assert_eq!(
            save_model_route(&store, unknown)
                .await
                .unwrap_err()
                .to_string(),
            "effort \"high\" is not supported for route target anthropic/claude-invented"
        );

        let mut empty = route("empty");
        empty.targets[0].effort = Some("  ".into());
        assert_eq!(
            save_model_route(&store, empty)
                .await
                .unwrap_err()
                .to_string(),
            "route target effort cannot be empty; use Model default"
        );
    }

    #[tokio::test]
    async fn invalid_stored_target_effort_remains_readable_but_cannot_be_saved() {
        let store = mem_store().await;
        let raw = r#"[{"id":"r1","name":"free","enabled":true,"strategy":"fallback","targets":[{"provider":"anthropic","model":"claude-opus-4-5","effort":"max"}],"createdAt":1,"updatedAt":1}]"#;
        store
            .set_setting(crate::domain::WriteOrigin::User, SETTING_KEY, raw)
            .await
            .unwrap();

        let stored = list_model_routes(&store).await.unwrap().pop().unwrap();
        assert_eq!(stored.targets[0].effort.as_deref(), Some("max"));
        assert_eq!(
            save_model_route(&store, stored)
                .await
                .unwrap_err()
                .to_string(),
            "effort \"max\" is not supported for route target anthropic/claude-opus-4-5"
        );
    }

    #[tokio::test]
    async fn route_target_capabilities_use_resolver_values_without_duplicate_models() {
        let store = mem_store().await;
        crate::llm_router::connections::add_connection(
            &store,
            crate::llm_router::connections::ConnectionRow {
                id: "anthropic-live".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Anthropic".into(),
                priority: 0,
                enabled: true,
                data: crate::llm_router::connections::ConnectionData {
                    models_override: Some(vec!["claude-opus-4-7".into()]),
                    ..Default::default()
                },
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let matching = list_model_route_target_capabilities(&store)
            .await
            .unwrap()
            .into_iter()
            .filter(|capability| {
                capability.provider == "anthropic" && capability.model == "claude-opus-4-7"
            })
            .collect::<Vec<_>>();
        assert_eq!(matching.len(), 1);
        assert_eq!(
            matching[0]
                .supported
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "medium", "high", "max", "xhigh"]
        );
        assert_eq!(matching[0].provider_default.as_deref(), Some("high"));
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
        let mut bad = route("free");
        bad.targets[0].provider = "anthropic-oauth".into(); // member, not head
        assert!(save_model_route(&store, bad).await.is_err());
        let mut unknown = route("free-2");
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
        let mut model_route = route("free");
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
