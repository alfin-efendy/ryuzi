//! Gateways screen commands: thin proxies to the engine daemon's gateways RPC
//! family. The local host is always first with live telemetry; WSL distros
//! are detected; SSH remotes are persisted config with a TCP reachability
//! probe. Remote execution needs the future daemon — these entries are
//! monitoring/config surfaces, and the UI says so.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use std::collections::HashSet;
use std::sync::Arc;
use tauri::{AppHandle, State};

// `GatewayResourceInfo` is only reachable transitively (as a field of
// `GatewayInfo::resources`) but is re-exported by name anyway for a
// complete, documented DTO surface; specta still emits it via the type
// graph either way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::{GatewayEventInfo, GatewayInfo, GatewayResourceInfo};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

#[tauri::command]
#[specta::specta]
pub async fn list_gateways(engine: Engine<'_>, runner_id: Option<String>) -> R<Vec<GatewayInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client.rpc("list_gateways", serde_json::json!({})).await
}

/// Live probe: local telemetry, WSL detection, and SSH TCP reachability.
#[tauri::command]
#[specta::specta]
pub async fn probe_gateways(engine: Engine<'_>, runner_id: Option<String>) -> R<Vec<GatewayInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client.rpc("probe_gateways", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn add_gateway(
    engine: Engine<'_>,
    runner_id: Option<String>,
    name: String,
    host: String,
    port: u16,
    username: String,
) -> R<Vec<GatewayInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "add_gateway",
            serde_json::json!({ "name": name, "host": host, "port": port, "username": username }),
        )
        .await
}

/// P3-6 "Add Runner" flow. The user copies Name/Host/Port/Fingerprint/code
/// off `ryuzi pair`'s printout on the REMOTE host and enters them here.
/// Three steps, all in Cockpit's Tauri backend (never core — core stays
/// free of outbound HTTP to arbitrary hosts):
///
/// 1. Pair over a pinned-TLS client trusting `fingerprint` (TOFU):
///    `POST https://{host}:{port}/pair {code, device_name}` ->
///    `{device_token}` ([`crate::engine::pair_over_pinned_tls`]).
/// 2. Persist the paired row via the LOCAL engine's `save_runner` RPC
///    (encrypts `device_token` at rest — see `gateways_api::save_runner`).
/// 3. Live-add the runner to the [`EngineManager`] (pinned client + SSE
///    bridge) so it's usable immediately, no Cockpit restart required.
///
/// SECURITY: `device_token` never leaves this function — it's consumed by
/// step 1's caller and handed straight to steps 2 and 3, never placed on
/// the `Vec<GatewayInfo>` this command returns (that DTO has no token
/// field) and never logged. See `engine_manager.rs`'s module docs for the
/// broader invariant this preserves.
#[tauri::command]
#[specta::specta]
pub async fn add_runner(
    app: AppHandle,
    engine: Engine<'_>,
    name: String,
    host: String,
    port: u16,
    fingerprint: String,
    code: String,
) -> R<Vec<GatewayInfo>> {
    let local = engine.client("local")?;

    // `save_runner` returns the full, freshly-assembled gateway list, not
    // the new row's id — diff against a pre-call snapshot of remote ids to
    // find it, so the live-add below targets the right runner. Taken BEFORE
    // pairing (not just before `save_runner`) so it never sits in the
    // failure window after the single-use pairing code has been consumed —
    // if this snapshot call fails, nothing has happened yet and the code is
    // still unredeemed.
    let before: Vec<GatewayInfo> = local.rpc("list_gateways", serde_json::json!({})).await?;
    let before_ids: HashSet<String> = before.into_iter().map(|g| g.id).collect();

    let base_url = format!("https://{host}:{port}");
    let device_token =
        crate::engine::pair_over_pinned_tls(&base_url, &fingerprint, &code, "Ryuzi Cockpit")
            .await?;

    let gateways: Vec<GatewayInfo> = local
        .rpc(
            "save_runner",
            serde_json::json!({
                "name": name,
                "host": host,
                "port": port,
                "fingerprint": fingerprint,
                "device_token": device_token,
            }),
        )
        .await?;

    if let Some(new_row) = gateways
        .iter()
        .find(|g| g.kind == "remote" && !before_ids.contains(&g.id))
    {
        engine.add_runner(
            new_row.id.clone(),
            host,
            port,
            device_token,
            fingerprint,
            &app,
        );
    }

    Ok(gateways)
}

#[tauri::command]
#[specta::specta]
pub async fn remove_gateway(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<Vec<GatewayInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("remove_gateway", serde_json::json!({ "id": id }))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_gateway(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
    fs_mode: String,
    paths: Vec<String>,
) -> R<Vec<GatewayInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc(
            "update_gateway",
            serde_json::json!({ "id": id, "fs_mode": fs_mode, "paths": paths }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn gateway_events(
    engine: Engine<'_>,
    runner_id: Option<String>,
    id: String,
) -> R<Vec<GatewayEventInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("gateway_events", serde_json::json!({ "id": id }))
        .await
}
