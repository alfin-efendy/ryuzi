//! Settings → Agent RPC family: the native agent's default model and
//! permission mode (settings KV via `crate::agent_settings`) plus the
//! selectable-model list the composer and Settings share. Proxied verbatim
//! from `apps/cockpit/src-tauri/src/agent_cmd.rs`.

use super::{ok, params, ApiError};
use crate::agent_settings::{self, AgentSettings};
use crate::api::types::AgentSettingsInfo;
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
    "get_agent_settings",
    "set_agent_settings",
    "list_selectable_models",
];

#[derive(Deserialize)]
struct SetAgentSettingsP {
    model: Option<String>,
    perm_mode: Option<String>,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "get_agent_settings" => {
            let s = agent_settings::get(cp.store()).await?;
            ok(AgentSettingsInfo {
                model: s.model,
                perm_mode: s.perm_mode,
            })
        }
        "set_agent_settings" => {
            let a: SetAgentSettingsP = params(p)?;
            agent_settings::set(
                cp.store(),
                &AgentSettings {
                    model: a.model,
                    perm_mode: a.perm_mode,
                },
            )
            .await?;
            ok(())
        }
        "list_selectable_models" => {
            ok(crate::llm_router::client::selectable_native_models(cp.store()).await)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;

    #[tokio::test]
    async fn set_then_get_round_trips_via_rpc() {
        let s = state().await;
        dispatch(
            &s,
            "set_agent_settings",
            json!({"model": "smart", "perm_mode": "plan"}),
        )
        .await
        .unwrap();
        let got = dispatch(&s, "get_agent_settings", json!({})).await.unwrap();
        assert_eq!(got["model"], "smart");
        assert_eq!(got["permMode"], "plan");
    }

    #[tokio::test]
    async fn list_selectable_models_dispatches() {
        let s = state().await;
        let out = dispatch(&s, "list_selectable_models", json!({}))
            .await
            .unwrap();
        assert!(out.is_array());
    }
}
