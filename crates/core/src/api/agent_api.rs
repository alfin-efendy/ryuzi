//! Settings → Agent RPC family: the native agent's default model and
//! permission mode (settings KV via `crate::agent_settings`) plus the
//! selectable-model list the composer and Settings share. Proxied verbatim
//! from `apps/cockpit/src-tauri/src/agent_cmd.rs`.

use super::{ok, params, ApiError};
use crate::agent_settings::{self, AgentSettings};
use crate::api::types::{AgentSettingsInfo, SessionRuntimeInfo};
use crate::llm_router::model_effort::{
    self, EffectiveEffortSource, ModelDefaultSource, ModelPreferenceKey, ProjectRuntimeInfo,
    StoredEffortStatus,
};
use crate::serve::ApiState;
use crate::{PermMode, SessionKind};
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
    "get_agent_settings",
    "set_agent_settings",
    "list_selectable_models",
    "update_project_perm_mode",
    "project_runtime_info",
    "update_project_runtime",
    "set_model_effort_preference",
    "session_runtime_info",
    "update_session_runtime",
];

#[derive(Deserialize)]
struct SetAgentSettingsP {
    model: Option<String>,
    perm_mode: Option<String>,
}

#[derive(Deserialize)]
struct ProjectP {
    project_id: String,
}

#[derive(Deserialize)]
struct UpdateProjectPermModeP {
    project_id: String,
    perm_mode: PermMode,
}

#[derive(Deserialize)]
struct UpdateProjectRuntimeP {
    project_id: String,
    model: Option<String>,
    effort: Option<String>,
}

#[derive(Deserialize)]
struct SetModelEffortPreferenceP {
    key: ModelPreferenceKey,
    effort: Option<String>,
}

#[derive(Deserialize)]
struct SessionP {
    session_pk: String,
}

#[derive(Deserialize)]
struct UpdateSessionRuntimeP {
    session_pk: String,
    model: Option<String>,
    effort: Option<String>,
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

async fn project_runtime_info(
    state: &ApiState,
    project_id: String,
) -> Result<ProjectRuntimeInfo, ApiError> {
    let project_id = project_id.trim().to_string();
    let project = state
        .cp
        .store()
        .get_project(&project_id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown project: {project_id}")))?;
    let model_info = match project.model.as_deref() {
        Some(model) => crate::llm_router::client::selectable_native_models(state.cp.store())
            .await?
            .into_iter()
            .find(|candidate| candidate.request_value == model),
        None => None,
    };
    let stored_effort_status = match (project.effort.as_deref(), model_info.as_ref()) {
        (None, _) => StoredEffortStatus::Valid,
        (Some(_), None) => StoredEffortStatus::UnknownMetadata,
        (Some(_), Some(info)) if info.supported.is_empty() => StoredEffortStatus::UnknownMetadata,
        (Some(effort), Some(info))
            if info.supported.iter().any(|option| option.value == effort) =>
        {
            StoredEffortStatus::Valid
        }
        (Some(_), Some(_)) => StoredEffortStatus::Unsupported,
    };
    let project_effort = project.effort.as_ref().filter(|effort| {
        model_info
            .as_ref()
            .is_some_and(|info| info.supported.iter().any(|option| &option.value == *effort))
    });
    let route_compatibility = match (project.model.as_deref(), model_info.as_ref()) {
        (Some(model), Some(info)) => {
            crate::llm_router::client::named_route_compatibility_default(
                state.cp.store(),
                model,
                &info.supported,
            )
            .await?
        }
        _ => None,
    };
    let (effective_effort, effective_source) = if let Some(effort) = project_effort {
        (Some(effort.clone()), EffectiveEffortSource::Project)
    } else if let Some(effort) = route_compatibility {
        (Some(effort), EffectiveEffortSource::RouteCompatibility)
    } else if let Some(info) = &model_info {
        let source = match info.default_source {
            ModelDefaultSource::Configured => EffectiveEffortSource::Configured,
            ModelDefaultSource::Provider => EffectiveEffortSource::Provider,
            ModelDefaultSource::VariesByTarget | ModelDefaultSource::None => {
                EffectiveEffortSource::None
            }
        };
        (info.resolved_default.clone(), source)
    } else {
        (None, EffectiveEffortSource::None)
    };
    let effective_effort_label = effective_effort.as_ref().and_then(|value| {
        model_info.as_ref().and_then(|info| {
            info.supported
                .iter()
                .find(|option| &option.value == value)
                .map(|option| option.label.clone())
        })
    });
    Ok(ProjectRuntimeInfo {
        project_id,
        model: project.model,
        stored_effort: project.effort,
        effective_effort,
        effective_effort_label,
        effective_source,
        stored_effort_status,
        model_info,
    })
}

async fn session_runtime_info(
    state: &ApiState,
    session_pk: String,
) -> Result<SessionRuntimeInfo, ApiError> {
    let session_pk = session_pk.trim().to_string();
    let session = state
        .cp
        .store()
        .get_session(&session_pk)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown session: {session_pk}")))?;
    if session.kind != SessionKind::Chat || session.project_id.is_some() {
        return Err(ApiError::bad_request(
            "session runtime settings are only available for projectless chats",
        ));
    }
    let runtime = state
        .cp
        .store()
        .get_session_runtime_settings(&session_pk)
        .await?
        .unwrap_or(crate::store::SessionRuntimeSettings {
            model: None,
            effort: None,
        });
    let model_info = match runtime.model.as_deref() {
        Some(model) => crate::llm_router::client::selectable_native_models(state.cp.store())
            .await?
            .into_iter()
            .find(|candidate| candidate.request_value == model),
        None => None,
    };
    let stored_effort_status = match (runtime.effort.as_deref(), model_info.as_ref()) {
        (None, _) => StoredEffortStatus::Valid,
        (Some(_), None) => StoredEffortStatus::UnknownMetadata,
        (Some(_), Some(info)) if info.supported.is_empty() => StoredEffortStatus::UnknownMetadata,
        (Some(effort), Some(info))
            if info.supported.iter().any(|option| option.value == effort) =>
        {
            StoredEffortStatus::Valid
        }
        _ => StoredEffortStatus::Unsupported,
    };
    let explicit = runtime.effort.as_ref().filter(|effort| {
        model_info
            .as_ref()
            .is_some_and(|info| info.supported.iter().any(|option| &option.value == *effort))
    });
    let route_compatibility = match (runtime.model.as_deref(), model_info.as_ref()) {
        (Some(model), Some(info)) => {
            crate::llm_router::client::named_route_compatibility_default(
                state.cp.store(),
                model,
                &info.supported,
            )
            .await?
        }
        _ => None,
    };
    let (effective_effort, effective_source) = if let Some(effort) = explicit {
        (Some(effort.clone()), EffectiveEffortSource::Session)
    } else if let Some(effort) = route_compatibility {
        (Some(effort), EffectiveEffortSource::RouteCompatibility)
    } else if let Some(info) = &model_info {
        let source = match info.default_source {
            ModelDefaultSource::Configured => EffectiveEffortSource::Configured,
            ModelDefaultSource::Provider => EffectiveEffortSource::Provider,
            _ => EffectiveEffortSource::None,
        };
        (info.resolved_default.clone(), source)
    } else {
        (None, EffectiveEffortSource::None)
    };
    let effective_effort_label = effective_effort.as_ref().and_then(|value| {
        model_info.as_ref().and_then(|info| {
            info.supported
                .iter()
                .find(|option| &option.value == value)
                .map(|option| option.label.clone())
        })
    });
    Ok(SessionRuntimeInfo {
        session_pk,
        model: runtime.model,
        stored_effort: runtime.effort,
        effective_effort,
        effective_effort_label,
        effective_source,
        stored_effort_status,
        model_info,
    })
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
            ok(crate::llm_router::client::selectable_native_models(cp.store()).await?)
        }
        "update_project_perm_mode" => {
            let a: UpdateProjectPermModeP = params(p)?;
            if !cp
                .store()
                .update_project_perm_mode(&a.project_id, a.perm_mode)
                .await?
            {
                return Err(ApiError::not_found(format!(
                    "unknown project: {}",
                    a.project_id
                )));
            }
            ok(())
        }
        "project_runtime_info" => {
            let a: ProjectP = params(p)?;
            ok(project_runtime_info(state, a.project_id).await?)
        }
        "update_project_runtime" => {
            let a: UpdateProjectRuntimeP = params(p)?;
            let project_id = a.project_id.trim().to_string();
            let model = clean_optional(a.model);
            let effort = match a.effort {
                Some(value) if value.trim().is_empty() => {
                    return Err(ApiError::bad_request("effort cannot be empty"));
                }
                value => clean_optional(value),
            };
            if cp.store().get_project(&project_id).await?.is_none() {
                return Err(ApiError::not_found(format!(
                    "unknown project: {project_id}"
                )));
            }
            if let Some(value) = effort.as_deref() {
                let selected = crate::llm_router::client::selectable_native_models(cp.store())
                    .await?
                    .into_iter()
                    .find(|candidate| model.as_deref() == Some(candidate.request_value.as_str()));
                if !selected.is_some_and(|selected| {
                    selected
                        .supported
                        .iter()
                        .any(|option| option.value == value)
                }) {
                    return Err(ApiError::bad_request(format!(
                        "effort {value:?} is not supported for the selected model"
                    )));
                }
            }
            cp.store()
                .update_project_runtime(&project_id, model, effort)
                .await?;
            ok(project_runtime_info(state, project_id).await?)
        }
        "set_model_effort_preference" => {
            let a: SetModelEffortPreferenceP = params(p)?;
            let key = ModelPreferenceKey {
                family: a.key.family.trim().to_string(),
                model: a.key.model.trim().to_string(),
            };
            let effort = match a.effort {
                Some(value) if value.trim().is_empty() => {
                    return Err(ApiError::bad_request("effort cannot be empty"));
                }
                value => clean_optional(value),
            };
            model_effort::set_preference(cp.store(), &key, effort.as_deref()).await?;
            ok(())
        }
        "session_runtime_info" => {
            let a: SessionP = params(p)?;
            ok(session_runtime_info(state, a.session_pk).await?)
        }
        "update_session_runtime" => {
            let a: UpdateSessionRuntimeP = params(p)?;
            let session_pk = a.session_pk.trim().to_string();
            let model = clean_optional(a.model);
            let effort = match a.effort {
                Some(value) if value.trim().is_empty() => {
                    return Err(ApiError::bad_request("effort cannot be empty"));
                }
                value => clean_optional(value),
            };
            if let Some(value) = effort.as_deref() {
                let selected = crate::llm_router::client::selectable_native_models(cp.store())
                    .await?
                    .into_iter()
                    .find(|candidate| model.as_deref() == Some(candidate.request_value.as_str()));
                if !selected.is_some_and(|selected| {
                    selected
                        .supported
                        .iter()
                        .any(|option| option.value == value)
                }) {
                    return Err(ApiError::bad_request(format!(
                        "effort {value:?} is not supported for the selected model"
                    )));
                }
            }
            // Validate session kind before the write.
            session_runtime_info(state, session_pk.clone()).await?;
            cp.store()
                .update_session_runtime_settings(&session_pk, model, effort)
                .await?;
            ok(session_runtime_info(state, session_pk).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::{dispatch, tests_support::state};
    use crate::domain::{PermMode, Project};
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

    #[tokio::test]
    async fn project_runtime_effort_and_permission_handlers_round_trip() {
        let s = state().await;
        s.cp.store()
            .insert_project(Project {
                project_id: "p1".into(),
                name: "demo".into(),
                workdir: "/tmp/demo".into(),
                source: None,
                model: None,
                effort: None,
                perm_mode: PermMode::Default,
                created_at: None,
                is_git: false,
            })
            .await
            .unwrap();

        let info = dispatch(&s, "project_runtime_info", json!({"project_id": "p1"}))
            .await
            .unwrap();
        assert_eq!(info["projectId"], "p1");
        assert_eq!(info["storedEffortStatus"], "valid");

        dispatch(
            &s,
            "update_project_perm_mode",
            json!({"project_id": "p1", "perm_mode": "bypassPermissions"}),
        )
        .await
        .unwrap();
        assert_eq!(
            s.cp.store()
                .get_project("p1")
                .await
                .unwrap()
                .unwrap()
                .perm_mode,
            PermMode::BypassPermissions
        );

        let updated = dispatch(
            &s,
            "update_project_runtime",
            json!({"project_id": "p1", "model": null, "effort": null}),
        )
        .await
        .unwrap();
        assert_eq!(updated["projectId"], "p1");

        dispatch(
            &s,
            "set_model_effort_preference",
            json!({"key": {"family": "openai", "model": "gpt-test"}, "effort": null}),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn project_runtime_handlers_reject_unknown_projects_and_empty_effort() {
        let s = state().await;
        let missing = dispatch(&s, "project_runtime_info", json!({"project_id": "missing"}))
            .await
            .unwrap_err();
        assert_eq!(missing.status, 404);

        let blank = dispatch(
            &s,
            "set_model_effort_preference",
            json!({"key": {"family": "openai", "model": "gpt"}, "effort": "  "}),
        )
        .await
        .unwrap_err();
        assert_eq!(blank.status, 400);
    }

    #[tokio::test]
    async fn projectless_session_runtime_handlers_round_trip() {
        use crate::domain::{Session, SessionKind, SessionStatus};
        let s = state().await;
        let chat = Session {
            session_pk: "chat-1".into(),
            project_id: None,
            agent_session_id: None,
            worktree_path: None,
            branch: None,
            title: None,
            status: SessionStatus::Idle,
            perm_mode: PermMode::Plan,
            started_by: None,
            created_at: None,
            last_active: None,
            resume_attempts: 0,
            branch_owned: false,
            kind: SessionKind::Chat,
            speaker: None,
            agent: None,
            parent_session_pk: None,
        };
        s.cp.store().insert_session(chat.clone()).await.unwrap();
        s.cp.store()
            .update_session_runtime_settings("chat-1", Some("route:smart".into()), None)
            .await
            .unwrap();

        let info = dispatch(&s, "session_runtime_info", json!({"session_pk": "chat-1"}))
            .await
            .unwrap();
        assert_eq!(info["sessionPk"], "chat-1");
        assert_eq!(info["model"], "route:smart");

        let updated = dispatch(
            &s,
            "update_session_runtime",
            json!({"session_pk": "chat-1", "model": null, "effort": null}),
        )
        .await
        .unwrap();
        assert!(updated["model"].is_null());

        for (pk, kind) in [
            ("worker-1", SessionKind::Worker),
            ("review-1", SessionKind::Review),
        ] {
            s.cp.store()
                .insert_session(Session {
                    session_pk: pk.into(),
                    kind,
                    ..chat.clone()
                })
                .await
                .unwrap();
            let err = dispatch(&s, "session_runtime_info", json!({"session_pk": pk}))
                .await
                .unwrap_err();
            assert_eq!(err.status, 400);
        }
    }
}
