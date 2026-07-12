//! Endpoint tab commands: thin proxies to the engine daemon's endpoint RPC
//! family (server lifecycle, port/autostart settings, keys, usage series).
//! The router server lives in the daemon now, so every command here drops
//! its old direct state extractor for it.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

// `UsagePoint` is only reachable transitively (as a field of
// `UsageSeries::days`) but is re-exported by name anyway for a complete,
// documented DTO surface; specta still emits it via the type graph either way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::{EndpointKeyInfo, EndpointStatusInfo, UsagePoint, UsageSeries};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn endpoint_status(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<EndpointStatusInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client.rpc("endpoint_status", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn start_endpoint(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<EndpointStatusInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client.rpc("start_endpoint", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn stop_endpoint(engine: Engine<'_>, runner_id: Option<String>) -> R<EndpointStatusInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client.rpc("stop_endpoint", serde_json::json!({})).await
}

/// Persist port + autostart; restart the server when it was running.
#[tauri::command]
#[specta::specta]
pub async fn set_endpoint_config(
    engine: Engine<'_>,
    runner_id: Option<String>,
    port: u16,
    autostart: bool,
) -> R<EndpointStatusInfo> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "set_endpoint_config",
            serde_json::json!({ "port": port, "autostart": autostart }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_endpoint_keys(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<Vec<EndpointKeyInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("list_endpoint_keys", serde_json::json!({}))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn create_endpoint_key(
    engine: Engine<'_>,
    runner_id: Option<String>,
    name: String,
) -> R<Vec<EndpointKeyInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("create_endpoint_key", serde_json::json!({ "name": name }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn revoke_endpoint_key(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<Vec<EndpointKeyInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("revoke_endpoint_key", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn connection_usage(
    engine: Engine<'_>,
    runner_id: Option<String>,
    connection_id: String,
    days: i64,
) -> R<UsageSeries> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "connection_usage",
            serde_json::json!({ "connection_id": connection_id, "days": days }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn endpoint_usage(
    engine: Engine<'_>,
    runner_id: Option<String>,
    days: i64,
) -> R<UsageSeries> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("endpoint_usage", serde_json::json!({ "days": days }))
        .await
}
