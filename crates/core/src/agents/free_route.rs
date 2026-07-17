//! Background construction of the `free` model route from MiMo/OpenCode free
//! models that pass a live probe. Kicked off once at daemon boot (after the
//! synchronous first-concrete seed) so startup stays fast and offline-safe.

use std::sync::Arc;

use crate::llm_router::routes::{self, ModelRouteInfo, ModelRouteStrategy, ModelRouteTarget};
use crate::llm_router::{connections, probe, registry};
use crate::store::Store;

/// The families whose free-tier models seed the `free` route.
const FREE_FAMILIES: &[&str] = &["mimo-free", "opencode-free"];

/// Probe every model on the enabled MiMo/OpenCode free connections and, when at
/// least one passes, overwrite the `free` route's targets with the passing set.
/// Returns the number of passing targets written (0 = route left untouched).
pub(crate) async fn rebuild_free_route(
    store: &Arc<Store>,
    http: &reqwest::Client,
) -> anyhow::Result<usize> {
    let conns = connections::list_connections(store).await?;
    let mut passing: Vec<ModelRouteTarget> = Vec::new();
    let mut seen: std::collections::HashSet<(String, String)> = std::collections::HashSet::new();
    for conn in conns.iter().filter(|c| c.enabled) {
        let Some(desc) = registry::descriptor(&conn.provider) else {
            continue;
        };
        if !FREE_FAMILIES.contains(&desc.family) {
            continue;
        }
        for model in connections::effective_models(desc, conn) {
            let outcome = probe::probe_model(http, store, desc, conn, &model).await;
            if outcome.ok {
                let key = (desc.family.to_string(), model.clone());
                if seen.insert(key) {
                    passing.push(ModelRouteTarget {
                        provider: desc.family.to_string(),
                        model,
                        effort: None,
                    });
                }
            }
        }
    }
    if passing.is_empty() {
        return Ok(0);
    }
    let count = passing.len();
    save_free_route(store, passing).await?;
    Ok(count)
}

/// Upsert the `free` route to exactly `targets` in a SINGLE store write.
///
/// Reusing any existing `free` route's id (and `created_at`) makes
/// [`routes::save_model_route`] update it in place — it upserts by id — rather
/// than delete-then-insert. That removes the transient window a two-step
/// delete+recreate had, where a crash between the two would leave `free`
/// missing until the next boot. When no `free` route exists yet, the empty id
/// inserts a fresh one.
async fn save_free_route(store: &Store, targets: Vec<ModelRouteTarget>) -> anyhow::Result<()> {
    let existing = routes::list_model_routes(store).await?;
    let free = existing
        .iter()
        .find(|r| r.name.eq_ignore_ascii_case("free"));
    routes::save_model_route(
        store,
        ModelRouteInfo {
            id: free.map(|r| r.id.clone()).unwrap_or_default(),
            name: "free".into(),
            enabled: true,
            strategy: ModelRouteStrategy::Fallback,
            targets,
            created_at: free.map(|r| r.created_at).unwrap_or(0),
            updated_at: 0,
        },
    )
    .await?;
    Ok(())
}

/// Spawn [`rebuild_free_route`] as a detached background task with a fresh
/// timeout-bounded HTTP client. Failures are logged, never propagated.
pub(crate) fn spawn_free_route_rebuild(store: Arc<Store>) {
    tokio::spawn(async move {
        let Ok(http) = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(20))
            .build()
        else {
            return;
        };
        match rebuild_free_route(&store, &http).await {
            Ok(0) => tracing::info!("free route: no free models passed the probe; kept baseline"),
            Ok(n) => tracing::info!(targets = n, "free route: rebuilt from probed free models"),
            Err(e) => tracing::warn!(error = %e, "free route: background rebuild failed"),
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::connections::{ConnectionData, ConnectionRow};

    #[tokio::test]
    async fn rebuild_is_noop_when_no_free_connections_exist() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        let http = reqwest::Client::new();
        let written = rebuild_free_route(&store, &http).await.unwrap();
        assert_eq!(written, 0);
        assert!(routes::list_model_routes(&store).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn rebuild_leaves_baseline_route_when_probes_fail_offline() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        // A pre-existing baseline `free` route (as the synchronous seed writes).
        routes::save_model_route(
            &store,
            ModelRouteInfo {
                id: String::new(),
                name: "free".into(),
                enabled: true,
                strategy: ModelRouteStrategy::Fallback,
                targets: vec![ModelRouteTarget {
                    provider: "mimo-free".into(),
                    model: "mimo-auto".into(),
                    effort: None,
                }],
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        connections::add_connection(
            &store,
            ConnectionRow {
                id: crate::paths::new_id(),
                provider: "mimo-free".into(),
                auth_type: "free".into(),
                label: "MiMo".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData::default(),
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        // Offline HTTP client → the probe errors (verdict not ok) → 0 written,
        // and the baseline route survives.
        let http = reqwest::Client::builder()
            .timeout(std::time::Duration::from_millis(1))
            .build()
            .unwrap();
        let written = rebuild_free_route(&store, &http).await.unwrap();
        assert_eq!(written, 0);
        let routes = routes::list_model_routes(&store).await.unwrap();
        assert!(routes.iter().any(|r| r.name == "free"));
    }

    #[tokio::test]
    async fn save_free_route_updates_the_existing_route_in_place() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        // A pre-existing baseline `free` route (as the synchronous seed writes).
        routes::save_model_route(
            &store,
            ModelRouteInfo {
                id: String::new(),
                name: "free".into(),
                enabled: true,
                strategy: ModelRouteStrategy::Fallback,
                targets: vec![ModelRouteTarget {
                    provider: "mimo-free".into(),
                    model: "mimo-auto".into(),
                    effort: None,
                }],
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        let baseline = routes::list_model_routes(&store).await.unwrap();
        let baseline_id = baseline
            .iter()
            .find(|r| r.name == "free")
            .unwrap()
            .id
            .clone();

        let next = vec![ModelRouteTarget {
            provider: "opencode-free".into(),
            model: "grok-code".into(),
            effort: None,
        }];
        save_free_route(&store, next.clone()).await.unwrap();

        let after = routes::list_model_routes(&store).await.unwrap();
        let frees: Vec<_> = after
            .iter()
            .filter(|r| r.name.eq_ignore_ascii_case("free"))
            .collect();
        // Exactly one `free` route, SAME id as before — proving an in-place
        // update rather than delete-then-recreate (so there is no window where
        // `free` is missing), with the new targets applied.
        assert_eq!(frees.len(), 1, "still exactly one free route");
        assert_eq!(frees[0].id, baseline_id, "id preserved: updated in place");
        assert_eq!(frees[0].targets, next);
    }

    #[tokio::test]
    async fn save_free_route_inserts_when_absent() {
        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        assert!(routes::list_model_routes(&store).await.unwrap().is_empty());

        save_free_route(
            &store,
            vec![ModelRouteTarget {
                provider: "mimo-free".into(),
                model: "mimo-auto".into(),
                effort: None,
            }],
        )
        .await
        .unwrap();

        let routes = routes::list_model_routes(&store).await.unwrap();
        assert_eq!(routes.len(), 1);
        assert_eq!(routes[0].name, "free");
        assert!(!routes[0].id.is_empty(), "insert assigned a fresh id");
    }
}
