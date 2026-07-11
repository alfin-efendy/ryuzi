//! Gateways screen commands: thin proxies to the engine daemon's gateways RPC
//! family. The local host is always first with live telemetry; WSL distros
//! are detected; SSH remotes are persisted config with a TCP reachability
//! probe. Remote execution needs the future daemon — these entries are
//! monitoring/config surfaces, and the UI says so.

use crate::engine::EngineClient;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

// `GatewayResourceInfo` is only reachable transitively (as a field of
// `GatewayInfo::resources`) but is re-exported by name anyway for a
// complete, documented DTO surface; specta still emits it via the type
// graph either way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::{GatewayEventInfo, GatewayInfo, GatewayResourceInfo};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineClient>>;

#[tauri::command]
#[specta::specta]
pub async fn list_gateways(engine: Engine<'_>) -> R<Vec<GatewayInfo>> {
    engine.rpc("list_gateways", serde_json::json!({})).await
}

/// Live probe: local telemetry, WSL detection, and SSH TCP reachability.
#[tauri::command]
#[specta::specta]
pub async fn probe_gateways(engine: Engine<'_>) -> R<Vec<GatewayInfo>> {
    engine.rpc("probe_gateways", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn add_gateway(
    engine: Engine<'_>,
    name: String,
    host: String,
    port: u16,
    username: String,
) -> R<Vec<GatewayInfo>> {
    engine
        .rpc(
            "add_gateway",
            serde_json::json!({ "name": name, "host": host, "port": port, "username": username }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn remove_gateway(engine: Engine<'_>, id: String) -> R<Vec<GatewayInfo>> {
    engine
        .rpc("remove_gateway", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_gateway(
    engine: Engine<'_>,
    id: String,
    fs_mode: String,
    paths: Vec<String>,
) -> R<Vec<GatewayInfo>> {
    engine
        .rpc(
            "update_gateway",
            serde_json::json!({ "id": id, "fs_mode": fs_mode, "paths": paths }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn gateway_events(engine: Engine<'_>, id: String) -> R<Vec<GatewayEventInfo>> {
    engine
        .rpc("gateway_events", serde_json::json!({ "id": id }))
        .await
}
