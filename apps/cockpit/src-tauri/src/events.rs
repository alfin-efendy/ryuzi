use ryuzi_core::CoreEvent;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri_specta::Event;

/// `runner_id` identifies which engine (`"local"` or a paired remote
/// runner's id — see `engine_manager::EngineManager`) produced this event.
/// Stamped by `engine_manager::spawn_bridge`, one instance of which runs per
/// runner, so the frontend can tell multiple runners' events apart once
/// P3-4/P3-6 route by runner.
#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct CoreEventMsg {
    pub runner_id: String,
    pub event: CoreEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct OauthAuthorizeUrlMsg {
    pub runner_id: String,
    pub provider: String,
    pub authorize_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct PluginOauthAuthorizeUrlMsg {
    pub runner_id: String,
    pub plugin_id: String,
    pub authorize_url: String,
}

/// Emitted by `begin_plugin_install`'s background callback task when the
/// loopback OAuth flow finishes: `ok: true` after the token is stored;
/// `ok: false` (with `error`) on timeout, state mismatch, or exchange
/// failure — the flow entry survives failures so manual paste still works.
#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct PluginOauthCompletedMsg {
    pub plugin_id: String,
    pub ok: bool,
    pub error: Option<String>,
}
