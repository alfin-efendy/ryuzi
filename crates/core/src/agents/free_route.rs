//! Background construction of the `free` model route from MiMo/OpenCode free
//! models that pass a live probe. Kicked off once at daemon boot (after the
//! synchronous first-concrete seed) so startup stays fast and offline-safe.

use std::sync::Arc;

use crate::llm_router::routes::{self, ModelRouteInfo, ModelRouteStrategy, ModelRouteTarget};
use crate::llm_router::{connections, models, probe, registry};
use crate::store::Store;

/// The families whose free-tier models seed the `free` route.
const FREE_FAMILIES: &[&str] = &["mimo-free", "opencode-free"];

/// Refresh each enabled MiMo/OpenCode free connection's live model catalog,
/// probe every model on it, and — when at least one passes — overwrite the
/// `free` route's targets with the passing set. Returns the number of passing
/// targets written (0 = route left untouched).
///
/// The refresh is what makes the route usable out of the box: a free provider
/// may seed no models at all and publish its catalog only from a live endpoint,
/// in which case there is nothing to probe until it has been fetched.
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
        let mut conn = conn.clone();
        // Discover the live catalog before probing. `opencode-free` seeds no
        // models at all (`models: &[]`) and publishes them from its own
        // endpoint, so without this there is nothing to probe and none of its
        // models can ever reach the route. The refresh persists what it finds,
        // so the Models view sees the same catalog. Non-fatal: a provider with
        // no endpoint (`mimo-free`) keeps its seeded list, and a failure here
        // (offline, 404) still leaves that list to probe.
        if desc.has_models_endpoint {
            if let Err(e) = models::refresh_connection_models(store, http, &mut conn).await {
                tracing::warn!(
                    provider = %conn.provider,
                    error = %e,
                    "free route: live model refresh failed; probing the seeded list"
                );
            }
        }
        for model in connections::effective_models(desc, &conn) {
            let outcome = probe::probe_model(http, store, desc, &conn, &model).await;
            // Persist the verdict exactly as the Models view's "Test All" does
            // (`connections_api::test_connection_model`), so a fresh install
            // shows every free model's status without a manual pass. Same
            // best-effort contract: `upsert_model_status` drops "unknown", so a
            // rate limit or outage never clobbers a stored valid/invalid, and a
            // store hiccup must not derail the rebuild.
            let _ = store
                .upsert_model_status(crate::store::ModelStatusRow {
                    family: desc.family.to_string(),
                    model: model.clone(),
                    status: outcome.status.as_str().to_string(),
                    message: outcome.message.clone(),
                    tested_at: crate::paths::now_ms(),
                })
                .await;
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

    /// `opencode-free` ships an EMPTY static model list (`models: &[]`) and
    /// discovers its catalog live (`has_models_endpoint: true`). Without a
    /// refresh first, `effective_models` returns nothing, so not one of its
    /// models is ever probed and none can reach the `free` route — a fresh
    /// install ends up routing to `mimo-auto` alone while every OpenCode free
    /// model sits unused and untested.
    #[tokio::test]
    async fn rebuild_refreshes_live_models_before_probing_so_opencode_free_can_route() {
        use axum::{routing::get, routing::post, Json, Router};
        use serde_json::json;

        let app = Router::new()
            .route(
                "/models",
                get(|| async { Json(json!({"data": [{"id": "free-a"}, {"id": "free-b"}]})) }),
            )
            .route(
                "/chat/completions",
                post(|| async {
                    Json(json!({
                        "id": "c1",
                        "choices": [{"index": 0, "message": {"role": "assistant", "content": "ok"}}]
                    }))
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let db = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db.path()).await.unwrap());
        connections::add_connection(
            &store,
            ConnectionRow {
                id: crate::paths::new_id(),
                provider: "opencode-free".into(),
                auth_type: "free".into(),
                label: "OpenCode (free)".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    base_url_override: Some(format!("http://127.0.0.1:{port}")),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();

        let http = reqwest::Client::new();
        let written = rebuild_free_route(&store, &http).await.unwrap();

        assert_eq!(
            written, 2,
            "both live-discovered models should pass the probe"
        );
        let routes = routes::list_model_routes(&store).await.unwrap();
        let free = routes
            .iter()
            .find(|r| r.name == "free")
            .expect("free route");
        let models: Vec<&str> = free.targets.iter().map(|t| t.model.as_str()).collect();
        assert_eq!(models, ["free-a", "free-b"]);
        assert!(free.targets.iter().all(|t| t.provider == "opencode-free"));

        // Every verdict is persisted exactly as the Models view's "Test All"
        // would, so a fresh install shows each free model's status without the
        // user running a manual pass first.
        let statuses = store.list_model_statuses("opencode-free").await.unwrap();
        let tested: Vec<(&str, &str)> = statuses
            .iter()
            .map(|s| (s.model.as_str(), s.status.as_str()))
            .collect();
        assert_eq!(tested, [("free-a", "valid"), ("free-b", "valid")]);
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
