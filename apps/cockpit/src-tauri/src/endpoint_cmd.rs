//! Endpoint tab commands: server lifecycle, port/autostart settings, keys.
use crate::error::CmdError;
use ryuzi_core::llm_router::keys;
use ryuzi_core::llm_router::server::{RouterServer, DEFAULT_PORT};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EndpointStatusInfo {
    pub running: bool,
    pub port: u16,
    pub base_url: String,
    pub autostart: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct EndpointKeyInfo {
    pub id: String,
    pub name: String,
    pub key: String,
    pub created_at: i64,
    pub last_used_at: Option<i64>,
}

pub async fn configured_port(cp: &ControlPlane) -> u16 {
    cp.store()
        .get_setting("endpoint_port")
        .await
        .ok()
        .flatten()
        .and_then(|s| s.parse().ok())
        .unwrap_or(DEFAULT_PORT)
}

async fn status_info(cp: &ControlPlane, srv: &RouterServer) -> EndpointStatusInfo {
    let st = srv.status();
    let port = if st.running { st.port } else { configured_port(cp).await };
    let autostart = cp
        .store()
        .get_setting("endpoint_autostart")
        .await
        .ok()
        .flatten()
        .map(|v| v == "1")
        .unwrap_or(false);
    EndpointStatusInfo {
        running: st.running,
        port,
        base_url: format!("http://127.0.0.1:{port}/v1"),
        autostart,
    }
}

#[tauri::command]
#[specta::specta]
pub async fn endpoint_status(
    cp: State<'_, Arc<ControlPlane>>,
    srv: State<'_, Arc<RouterServer>>,
) -> R<EndpointStatusInfo> {
    Ok(status_info(&cp, &srv).await)
}

#[tauri::command]
#[specta::specta]
pub async fn start_endpoint(
    cp: State<'_, Arc<ControlPlane>>,
    srv: State<'_, Arc<RouterServer>>,
) -> R<EndpointStatusInfo> {
    let port = configured_port(&cp).await;
    srv.start(port).await.map_err(|e| CmdError { message: e.to_string() })?;
    Ok(status_info(&cp, &srv).await)
}

#[tauri::command]
#[specta::specta]
pub async fn stop_endpoint(
    cp: State<'_, Arc<ControlPlane>>,
    srv: State<'_, Arc<RouterServer>>,
) -> R<EndpointStatusInfo> {
    srv.stop().await;
    Ok(status_info(&cp, &srv).await)
}

/// Persist port + autostart; restart the server when it was running.
#[tauri::command]
#[specta::specta]
pub async fn set_endpoint_config(
    cp: State<'_, Arc<ControlPlane>>,
    srv: State<'_, Arc<RouterServer>>,
    port: u16,
    autostart: bool,
) -> R<EndpointStatusInfo> {
    cp.store().set_setting("endpoint_port", &port.to_string()).await?;
    cp.store()
        .set_setting("endpoint_autostart", if autostart { "1" } else { "0" })
        .await?;
    if srv.status().running {
        srv.start(port).await.map_err(|e| CmdError { message: e.to_string() })?;
    }
    Ok(status_info(&cp, &srv).await)
}

fn to_key_info(k: keys::EndpointKey) -> EndpointKeyInfo {
    EndpointKeyInfo {
        id: k.id,
        name: k.name,
        key: k.key,
        created_at: k.created_at,
        last_used_at: k.last_used_at,
    }
}

#[tauri::command]
#[specta::specta]
pub async fn list_endpoint_keys(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<EndpointKeyInfo>> {
    Ok(keys::list_keys(cp.store()).await?.into_iter().map(to_key_info).collect())
}

#[tauri::command]
#[specta::specta]
pub async fn create_endpoint_key(
    cp: State<'_, Arc<ControlPlane>>,
    name: String,
) -> R<Vec<EndpointKeyInfo>> {
    keys::create_key(cp.store(), &name).await?;
    Ok(keys::list_keys(cp.store()).await?.into_iter().map(to_key_info).collect())
}

#[tauri::command]
#[specta::specta]
pub async fn revoke_endpoint_key(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
) -> R<Vec<EndpointKeyInfo>> {
    keys::revoke_key(cp.store(), &id).await?;
    Ok(keys::list_keys(cp.store()).await?.into_iter().map(to_key_info).collect())
}
