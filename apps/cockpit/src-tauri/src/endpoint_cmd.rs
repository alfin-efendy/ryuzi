//! Endpoint tab commands: thin proxies to the engine daemon's endpoint RPC
//! family (server lifecycle, port/autostart settings, keys, usage series).
//! The router server lives in the daemon now, so every command here drops
//! its old direct state extractor for it.

use crate::engine::EngineClient;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

// `UsagePoint` is only reachable transitively (as a field of
// `UsageSeries::days`) but is re-exported by name anyway for a complete,
// documented DTO surface; specta still emits it via the type graph either way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::{EndpointKeyInfo, EndpointStatusInfo, UsagePoint, UsageSeries};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineClient>>;

#[tauri::command]
#[specta::specta]
pub async fn endpoint_status(engine: Engine<'_>) -> R<EndpointStatusInfo> {
    engine.rpc("endpoint_status", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn start_endpoint(engine: Engine<'_>) -> R<EndpointStatusInfo> {
    engine.rpc("start_endpoint", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn stop_endpoint(engine: Engine<'_>) -> R<EndpointStatusInfo> {
    engine.rpc("stop_endpoint", serde_json::json!({})).await
}

/// Persist port + autostart; restart the server when it was running.
#[tauri::command]
#[specta::specta]
pub async fn set_endpoint_config(
    engine: Engine<'_>,
    port: u16,
    autostart: bool,
) -> R<EndpointStatusInfo> {
    engine
        .rpc(
            "set_endpoint_config",
            serde_json::json!({ "port": port, "autostart": autostart }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_endpoint_keys(engine: Engine<'_>) -> R<Vec<EndpointKeyInfo>> {
    engine
        .rpc("list_endpoint_keys", serde_json::json!({}))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn create_endpoint_key(engine: Engine<'_>, name: String) -> R<Vec<EndpointKeyInfo>> {
    engine
        .rpc("create_endpoint_key", serde_json::json!({ "name": name }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn revoke_endpoint_key(engine: Engine<'_>, id: String) -> R<Vec<EndpointKeyInfo>> {
    engine
        .rpc("revoke_endpoint_key", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn connection_usage(
    engine: Engine<'_>,
    connection_id: String,
    days: i64,
) -> R<UsageSeries> {
    engine
        .rpc(
            "connection_usage",
            serde_json::json!({ "connection_id": connection_id, "days": days }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn endpoint_usage(engine: Engine<'_>, days: i64) -> R<UsageSeries> {
    engine
        .rpc("endpoint_usage", serde_json::json!({ "days": days }))
        .await
}
