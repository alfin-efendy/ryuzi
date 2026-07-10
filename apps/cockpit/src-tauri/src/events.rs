use ryuzi_core::CoreEvent;
use serde::{Deserialize, Serialize};
use specta::Type;
use tauri_specta::Event;

#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
pub struct CoreEventMsg {
    pub event: CoreEvent,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct OauthAuthorizeUrlMsg {
    pub provider: String,
    pub authorize_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, Event)]
#[serde(rename_all = "camelCase")]
pub struct PluginOauthAuthorizeUrlMsg {
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
