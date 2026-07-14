//! Automation Hook management RPCs.

use super::{ok, params, ApiError};
use crate::api::types::{AutomationHookDetail, AutomationHookInfo, AutomationHookInput};
use crate::automation::{self, AutomationEnvelope, AutomationSource, HookActionInput, TriggerKind};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::{json, Value};

pub(crate) const HANDLES: &[&str] = &[
    "list_automation_hooks",
    "automation_hook_detail",
    "create_automation_hook",
    "update_automation_hook",
    "toggle_automation_hook",
    "delete_automation_hook",
    "test_automation_hook",
];

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InputP {
    input: AutomationHookInput,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdInputP {
    id: String,
    input: AutomationHookInput,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdEnabledP {
    id: String,
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct IdP {
    id: String,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    match method {
        "list_automation_hooks" => ok(list(state).await?),
        "automation_hook_detail" => {
            let a: IdP = params(p)?;
            ok(detail(state, &a.id).await?)
        }
        "create_automation_hook" => {
            let a: InputP = params(p)?;
            let hook = automation::create_hook(state.cp.store(), a.input.into())
                .await
                .map_err(hook_mutation_error)?;
            ok(AutomationHookInfo::from(hook))
        }
        "update_automation_hook" => {
            let a: IdInputP = params(p)?;
            let hook = automation::update_hook(state.cp.store(), &a.id, a.input.into())
                .await
                .map_err(hook_mutation_error)?;
            ok(AutomationHookInfo::from(hook))
        }
        "toggle_automation_hook" => {
            let a: IdEnabledP = params(p)?;
            automation::toggle_hook(state.cp.store(), &a.id, a.enabled).await?;
            ok(detail(state, &a.id).await?.hook)
        }
        "delete_automation_hook" => {
            let a: IdP = params(p)?;
            automation::delete_hook(state.cp.store(), &a.id).await?;
            ok(list(state).await?)
        }
        "test_automation_hook" => {
            let a: IdP = params(p)?;
            ok(test_hook(state, &a.id).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

async fn list(state: &ApiState) -> Result<Vec<AutomationHookInfo>, ApiError> {
    Ok(automation::list_hooks(state.cp.store())
        .await?
        .into_iter()
        .map(Into::into)
        .collect())
}

async fn detail(state: &ApiState, id: &str) -> Result<AutomationHookDetail, ApiError> {
    automation::hook_detail(state.cp.store(), id)
        .await?
        .map(Into::into)
        .ok_or_else(|| ApiError::not_found(format!("automation hook not found: {id}")))
}

/// Client-input validation failures (unknown project/agent/model, malformed
/// fields) map to `400`; genuine store/I/O failures fall through to the
/// crate-wide `anyhow::Error -> ApiError` `500` mapping.
fn hook_mutation_error(error: automation::HookMutationError) -> ApiError {
    match error {
        automation::HookMutationError::Validation(message) => ApiError::bad_request(message),
        automation::HookMutationError::Store(error) => error.into(),
    }
}

async fn test_hook(state: &ApiState, id: &str) -> Result<AutomationHookDetail, ApiError> {
    let hook = automation::hook_detail(state.cp.store(), id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("automation hook not found: {id}")))?
        .hook;
    if !matches!(hook.action, HookActionInput::WebhookOutbound(_)) {
        return Err(ApiError::bad_request(
            "only webhook.outbound automation hooks can be tested",
        ));
    }

    let envelope = AutomationEnvelope::new(
        TriggerKind::SessionEnd,
        "2026-01-01T00:00:00Z",
        AutomationSource::new("automation.test", "cockpit"),
        json!({ "message": "This is a test automation delivery." }),
    );
    let run = automation::create_run(
        state.cp.store(),
        &hook.id,
        serde_json::to_value(envelope).map_err(|error| ApiError {
            status: 500,
            message: error.to_string(),
        })?,
    )
    .await?;
    let result = automation::deliver_outbound_test(state.cp.store(), &run).await;
    let (status, error) = match result {
        Ok(()) => ("success", None),
        Err(error) => ("failed", Some(error.to_string())),
    };
    automation::finish_run(state.cp.store(), &run.id, status, None, error.as_deref()).await?;
    detail(state, id).await
}

#[cfg(test)]
mod tests {
    use crate::api::{dispatch, tests_support::state, tests_support::state_with_project};
    use axum::{extract::Json, http::StatusCode, routing::post, Router};
    use serde_json::{json, Value};
    use std::sync::{Arc, Mutex};
    use tokio::sync::oneshot;

    async fn loopback_payload_server() -> (String, oneshot::Receiver<Value>, oneshot::Sender<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (payload_sender, payload) = oneshot::channel();
        let payload_sender = Arc::new(Mutex::new(Some(payload_sender)));
        let handler_payload_sender = Arc::clone(&payload_sender);
        let app = Router::new().route(
            "/",
            post(move |Json(payload): Json<Value>| {
                let payload_sender = Arc::clone(&handler_payload_sender);
                async move {
                    if let Some(sender) = payload_sender.lock().unwrap().take() {
                        let _ = sender.send(payload);
                    }
                    StatusCode::NO_CONTENT
                }
            }),
        );
        let (shutdown, receiver) = oneshot::channel();
        tokio::spawn(async move {
            axum::serve(listener, app)
                .with_graceful_shutdown(async { _ = receiver.await })
                .await
                .unwrap();
        });
        (format!("http://{address}/"), payload, shutdown)
    }

    fn valid_agent_hook_input() -> serde_json::Value {
        json!({
            "name": "Session follow-up",
            "triggerKind": "session.end",
            "action": {
                "kind": "agent.run",
                "config": {
                    "projectId": "p1",
                    "branch": "",
                    "gatewayId": "local",
                    "prompt": "Review the completed session",
                    "agentId": null,
                    "modelOverride": null,
                    "subtask": false
                }
            },
            "enabled": true
        })
    }

    #[tokio::test]
    async fn hook_crud_routes_through_core_rpc() {
        let state = state_with_project().await;
        let created = dispatch(
            &state,
            "create_automation_hook",
            json!({ "input": valid_agent_hook_input() }),
        )
        .await
        .unwrap();
        let id = created["id"].as_str().unwrap().to_string();

        let list = dispatch(&state, "list_automation_hooks", json!({}))
            .await
            .unwrap();
        assert_eq!(list.as_array().unwrap()[0]["id"], id);

        let detail = dispatch(&state, "automation_hook_detail", json!({ "id": id }))
            .await
            .unwrap();
        assert_eq!(detail["hook"]["name"], "Session follow-up");

        let updated_input = json!({
            "name": "Updated session follow-up",
            "triggerKind": "session.end",
            "action": valid_agent_hook_input()["action"].clone(),
            "enabled": true
        });
        let updated = dispatch(
            &state,
            "update_automation_hook",
            json!({ "id": id, "input": updated_input }),
        )
        .await
        .unwrap();
        assert_eq!(updated["name"], "Updated session follow-up");

        let toggled = dispatch(
            &state,
            "toggle_automation_hook",
            json!({ "id": id, "enabled": false }),
        )
        .await
        .unwrap();
        assert!(!toggled["enabled"].as_bool().unwrap());

        let deleted = dispatch(&state, "delete_automation_hook", json!({ "id": id }))
            .await
            .unwrap();
        assert!(deleted.as_array().unwrap().is_empty());
    }

    #[tokio::test]
    async fn test_automation_hook_delivers_a_successful_outbound_test_run() {
        let state = state().await;
        let (url, payload, shutdown) = loopback_payload_server().await;
        let created = dispatch(
            &state,
            "create_automation_hook",
            json!({
                "input": {
                    "name": "Test outbound webhook",
                    "triggerKind": "session.end",
                    "action": {
                        "kind": "webhook.outbound",
                        "config": {
                            "url": url,
                            "method": "POST",
                            "headers": [],
                            "payloadTemplate": r#"{ "run": "${run}" }"#
                        }
                    },
                    "enabled": true
                }
            }),
        )
        .await
        .unwrap();

        let detail = dispatch(
            &state,
            "test_automation_hook",
            json!({ "id": created["id"] }),
        )
        .await
        .unwrap();

        let latest_run = &detail["runs"][0];
        assert_eq!(latest_run["status"], "success");
        assert_eq!(latest_run["attemptCount"], 1);
        assert_eq!(latest_run["lastHttpStatus"], 204);
        assert_eq!(latest_run["attempts"][0]["httpStatus"], 204);
        assert_eq!(payload.await.unwrap()["run"]["test"], true);
        let _ = shutdown.send(());
    }

    #[tokio::test]
    async fn test_automation_hook_rejects_non_outbound_actions() {
        let state = state_with_project().await;
        let created = dispatch(
            &state,
            "create_automation_hook",
            json!({ "input": valid_agent_hook_input() }),
        )
        .await
        .unwrap();

        let error = dispatch(
            &state,
            "test_automation_hook",
            json!({ "id": created["id"] }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, 400);
        assert!(error.message.contains("webhook.outbound"));
    }
}
