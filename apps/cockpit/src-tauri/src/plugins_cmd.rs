//! Plugins screen commands: thin proxies to the engine daemon's plugins RPC
//! family — every installed plugin's identity/capabilities (`list_plugins`),
//! a single plugin's full detail (`plugin_detail`), enable/disable
//! (`set_plugin_enabled`), a validated settings write
//! (`set_plugin_setting`), plugin OAuth sign-in, and a provider's effective
//! model list (`plugin_models`).
//!
//! Behavior change from the pre-daemon version: `begin_plugin_oauth` no
//! longer takes an `AppHandle` or opens the system browser directly — the
//! engine broadcasts `CoreEvent::PluginOauthAuthorizeUrl` over SSE, and the
//! bridge in `lib.rs` opens the browser (and re-emits the legacy Tauri
//! event) on receipt.

use crate::engine::EngineClient;
use crate::error::CmdError;
use std::sync::Arc;
use tauri::State;

// `PluginFieldInfo`/`PluginMcpInfo` are only reachable transitively (as
// fields of `PluginDetail`) but are re-exported by name anyway for a
// complete, documented DTO surface; specta still emits them via the type
// graph either way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::{
    PluginAuthInfo, PluginDetail, PluginFieldInfo, PluginInfo, PluginMcpInfo,
    PluginOauthBeginResult,
};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineClient>>;

#[tauri::command]
#[specta::specta]
pub async fn list_plugins(engine: Engine<'_>) -> R<Vec<PluginInfo>> {
    engine.rpc("list_plugins", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn plugin_detail(engine: Engine<'_>, id: String) -> R<PluginDetail> {
    engine
        .rpc("plugin_detail", serde_json::json!({ "id": id }))
        .await
}

/// Same semantics as `ryuzi plugins enable/disable` — delegates to the
/// shared core helper so the two surfaces never drift.
#[tauri::command]
#[specta::specta]
pub async fn set_plugin_enabled(engine: Engine<'_>, id: String, enabled: bool) -> R<()> {
    engine
        .rpc(
            "set_plugin_enabled",
            serde_json::json!({ "id": id, "enabled": enabled }),
        )
        .await
}

/// Validated write through `SettingsStore::set` — rejects unknown keys and
/// type-mismatched values the same way `ryuzi config set` does. Never
/// returns a value, so no secret can leak back through this command.
#[tauri::command]
#[specta::specta]
pub async fn set_plugin_setting(engine: Engine<'_>, key: String, value: String) -> R<()> {
    engine
        .rpc(
            "set_plugin_setting",
            serde_json::json!({ "key": key, "value": value }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn begin_plugin_oauth(
    engine: Engine<'_>,
    plugin_id: String,
) -> R<PluginOauthBeginResult> {
    engine
        .rpc(
            "begin_plugin_oauth",
            serde_json::json!({ "plugin_id": plugin_id }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn complete_plugin_oauth(
    engine: Engine<'_>,
    plugin_id: String,
    code: String,
    state_token: String,
) -> R<PluginAuthInfo> {
    engine
        .rpc(
            "complete_plugin_oauth",
            serde_json::json!({ "plugin_id": plugin_id, "code": code, "state_token": state_token }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn disconnect_plugin_oauth(engine: Engine<'_>, plugin_id: String) -> R<PluginAuthInfo> {
    engine
        .rpc(
            "disconnect_plugin_oauth",
            serde_json::json!({ "plugin_id": plugin_id }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn plugin_models(engine: Engine<'_>, id: String) -> R<Vec<String>> {
    engine
        .rpc("plugin_models", serde_json::json!({ "id": id }))
        .await
}
