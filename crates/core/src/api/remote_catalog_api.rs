//! Remote plugin catalog RPC family: `refresh_catalog` fetches, verifies, and
//! caches the signed integration feed right now (instead of waiting for
//! `RemoteCatalogManager`'s ~6h cadence — see
//! `crate::plugins::remote_catalog`); `catalog_status` reports the last
//! accepted feed's sequence/outcome plus cached entry/blocked counts, both
//! params-free.
//!
//! `refresh_catalog` deliberately does NOT go through `RemoteCatalogManager`
//! (that type lives daemon-side, owns a background timer, and isn't reachable
//! from an `ApiState`): it drives the same `fetch_and_cache` +
//! change-detection + `apply_blocked_denylist` sequence directly against the
//! `Store`, mirroring `RemoteCatalogManager::refresh_verified` step for step
//! — including reusing [`crate::plugins::remote_catalog::catalog_content`]'s
//! content projection for change detection, so an unchanged re-fetch here
//! can never spuriously flip `plugins_restart_required` any more than the
//! background cadence can.

use super::{ok, ApiError};
use crate::api::types::CatalogStatus;
use crate::control::ControlPlane;
use crate::plugins::remote_catalog::{
    catalog_content, fetch_and_cache, ReqwestCatalogHttp, DEFAULT_CATALOG_FEED_URL,
};
use crate::serve::ApiState;
use crate::settings::SettingsStore;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &["refresh_catalog", "catalog_status"];

pub(crate) async fn dispatch(state: &ApiState, method: &str, _p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "refresh_catalog" => ok(refresh_catalog(cp).await?),
        "catalog_status" => ok(catalog_status(cp).await?),
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

/// Fetch the feed once, apply it, and — only when the fetch actually applied
/// AND the cached set's content changed — mark the daemon dirty and re-run
/// the blocked-id denylist sweep. A failed/rejected fetch (404, bad
/// signature, rollback) never marks a restart or re-sweeps: nothing on disk
/// changed.
async fn refresh_catalog(cp: &ControlPlane) -> anyhow::Result<CatalogStatus> {
    let settings = SettingsStore::new(cp.store().clone());
    let url = settings
        .get("catalog_feed_url")
        .await
        .ok()
        .flatten()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| DEFAULT_CATALOG_FEED_URL.to_string());
    let http = ReqwestCatalogHttp::new();

    let before = catalog_content(&cp.store().list_remote_catalog().await.unwrap_or_default());
    let outcome = fetch_and_cache(cp.store(), &http, &url).await;
    if outcome.applied {
        let after = catalog_content(&cp.store().list_remote_catalog().await.unwrap_or_default());
        if after != before {
            cp.mark_plugins_restart_required();
        }
        let _ = crate::plugins::apply_blocked_denylist(cp.store(), &settings, cp.plugins()).await;
    }
    catalog_status(cp).await
}

async fn catalog_status(cp: &ControlPlane) -> anyhow::Result<CatalogStatus> {
    let state = cp.store().get_catalog_feed_state().await?;
    let rows = cp.store().list_remote_catalog().await?;
    Ok(CatalogStatus {
        sequence: state.as_ref().map(|(s, _, _)| *s).unwrap_or(0),
        last_fetch_at: state.as_ref().map(|(_, a, _)| *a),
        outcome: state.map(|(_, _, o)| o),
        entries: rows.iter().filter(|r| !r.blocked).count() as u32,
        blocked: rows.iter().filter(|r| r.blocked).count() as u32,
    })
}

#[cfg(test)]
mod tests {
    use crate::api::{dispatch, tests_support::state};
    use crate::store::RemoteCatalogRow;
    use serde_json::json;

    #[tokio::test]
    async fn catalog_status_reports_seeded_feed_state_and_row_counts() {
        let s = state().await;
        s.cp.store().set_catalog_feed_state(9, "ok").await.unwrap();
        s.cp.store()
            .upsert_remote_catalog(&[
                RemoteCatalogRow {
                    id: "acme".to_string(),
                    manifest_toml: "contract=1\nid=\"acme\"\nname=\"Acme\"\nversion=\"1.0.0\""
                        .to_string(),
                    version: "1.0.0".to_string(),
                    sequence: 9,
                    blocked: false,
                    blocked_reason: None,
                    fetched_at: 0,
                },
                RemoteCatalogRow {
                    id: "evil".to_string(),
                    manifest_toml: String::new(),
                    version: String::new(),
                    sequence: 9,
                    blocked: true,
                    blocked_reason: Some("revoked".to_string()),
                    fetched_at: 0,
                },
            ])
            .await
            .unwrap();

        let out = dispatch(&s, "catalog_status", json!({})).await.unwrap();
        assert_eq!(out["sequence"], json!(9));
        // `set_catalog_feed_state` stamps `updated_at` with the real wall
        // clock (`crate::paths::now_ms()`), not an injectable value — assert
        // presence/positivity rather than pinning an exact timestamp.
        assert!(out["lastFetchAt"].as_i64().unwrap() > 0);
        assert_eq!(out["outcome"], json!("ok"));
        assert_eq!(out["entries"], json!(1));
        assert_eq!(out["blocked"], json!(1));
    }

    // No feed has ever been fetched: `get_catalog_feed_state` returns `None`,
    // so the DTO must fall back to sequence 0 / no outcome rather than error.
    #[tokio::test]
    async fn catalog_status_defaults_when_no_feed_state_recorded() {
        let s = state().await;
        let out = dispatch(&s, "catalog_status", json!({})).await.unwrap();
        assert_eq!(out["sequence"], json!(0));
        assert_eq!(out["lastFetchAt"], json!(null));
        assert_eq!(out["outcome"], json!(null));
        assert_eq!(out["entries"], json!(0));
        assert_eq!(out["blocked"], json!(0));
    }
}
