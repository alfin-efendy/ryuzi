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
