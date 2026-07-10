//! Settings → Agent commands: the native agent's default model and
//! permission mode (settings KV via ryuzi_core::agent_settings) plus the
//! selectable-model list the composer and Settings share.

use crate::error::CmdError;
use ryuzi_core::agent_settings::{self, AgentSettings};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentSettingsInfo {
    pub model: Option<String>,
    /// "plan" | "ask" | "edit" | "full"; None = engine default ("ask").
    pub perm_mode: Option<String>,
}

#[tauri::command]
#[specta::specta]
pub async fn get_agent_settings(cp: State<'_, Arc<ControlPlane>>) -> R<AgentSettingsInfo> {
    let s = agent_settings::get(cp.store()).await?;
    Ok(AgentSettingsInfo {
        model: s.model,
        perm_mode: s.perm_mode,
    })
}

#[tauri::command]
#[specta::specta]
pub async fn set_agent_settings(
    cp: State<'_, Arc<ControlPlane>>,
    model: Option<String>,
    perm_mode: Option<String>,
) -> R<()> {
    agent_settings::set(cp.store(), &AgentSettings { model, perm_mode }).await?;
    Ok(())
}

/// The models a native session can actually run, in presentation order —
/// enabled route aliases first, then provider/model ids (previously
/// embedded in list_runtimes' native entry).
#[tauri::command]
#[specta::specta]
pub async fn list_selectable_models(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<String>> {
    Ok(ryuzi_core::llm_router::client::selectable_native_models(cp.store()).await)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn set_then_get_round_trips_through_the_control_plane() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = ryuzi_core::Store::open(tmp.path()).await.unwrap();
        let cp = ryuzi_core::ControlPlane::new(store, ryuzi_core::Registries::new()).await;
        agent_settings::set(
            cp.store(),
            &AgentSettings {
                model: Some("smart".into()),
                perm_mode: Some("plan".into()),
            },
        )
        .await
        .unwrap();
        let got = agent_settings::get(cp.store()).await.unwrap();
        assert_eq!(got.model.as_deref(), Some("smart"));
        assert_eq!(got.perm_mode.as_deref(), Some("plan"));
    }
}
