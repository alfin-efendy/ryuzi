//! Settings → Agent RPC family: the native agent's default model and
//! permission mode (settings KV via `crate::agent_settings`) plus the
//! selectable-model list the composer and Settings share, and the YAML
//! agent registry surface (CRUD, duplicate, default selection, and the
//! shared subagent model) backed by `state.agents`.

use super::{ok, params, ApiError};
use crate::agent_settings::{self, AgentSettings};
use crate::agents::types::{
    AgentAvatar, AgentLoop, AgentModel, AgentMutationInput, AgentPermissionMode, AgentPermissions,
    AgentRegistrySnapshot, AgentSnapshot, AgentTools, PermissionDecision, PermissionRule,
};
use crate::api::types::{
    AgentDetailInfo, AgentModelInfo, AgentMutationInfo, AgentRecoveryInfo, AgentRegistryInfo,
    AgentSettingsInfo, AgentSummaryInfo, AgentValidationInfo, PermissionRuleInfo,
    SessionRuntimeInfo,
};
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
    "list_agents",
    "get_agent",
    "create_agent",
    "update_agent",
    "duplicate_agent",
    "delete_agent",
    "set_default_agent",
    "get_subagent_model",
    "update_subagent_model",
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

#[derive(Deserialize)]
struct AgentIdP {
    agent_id: String,
}

#[derive(Deserialize)]
struct CreateAgentP {
    input: AgentMutationInfo,
}

#[derive(Deserialize)]
struct UpdateAgentP {
    agent_id: String,
    input: AgentMutationInfo,
}

#[derive(Deserialize)]
struct UpdateSubagentModelP {
    model: AgentModelInfo,
}

impl From<AgentModel> for AgentModelInfo {
    fn from(model: AgentModel) -> Self {
        match model {
            AgentModel::Concrete { name, effort } => AgentModelInfo::Concrete { name, effort },
            AgentModel::Route { route } => AgentModelInfo::Route { route },
        }
    }
}

impl TryFrom<AgentModelInfo> for AgentModel {
    type Error = ApiError;

    fn try_from(info: AgentModelInfo) -> Result<Self, Self::Error> {
        match info {
            AgentModelInfo::Concrete { name, effort } => {
                let name = name.trim().to_owned();
                if name.is_empty() {
                    return Err(ApiError::bad_request("model name cannot be blank"));
                }
                let effort = effort.map(|value| value.trim().to_owned());
                if effort.as_deref() == Some("") {
                    return Err(ApiError::bad_request("model effort cannot be blank"));
                }
                Ok(AgentModel::Concrete { name, effort })
            }
            AgentModelInfo::Route { route } => {
                let route = route.trim().to_owned();
                if route.is_empty() {
                    return Err(ApiError::bad_request("model route cannot be blank"));
                }
                Ok(AgentModel::Route { route })
            }
        }
    }
}

/// The YAML permission-mode vocabulary as the wire strings the DTOs carry.
/// Mirrors `AgentPermissionMode`'s serde names exactly.
fn parse_permission_mode(value: &str) -> Result<PermMode, ApiError> {
    serde_json::from_value::<AgentPermissionMode>(Value::String(value.trim().to_owned()))
        .map(AgentPermissionMode::runtime_mode)
        .map_err(|_| {
            ApiError::bad_request(format!(
                "unknown permission mode `{value}` (expected ask, accept_edits, full, or plan)"
            ))
        })
}

fn permission_mode_string(mode: PermMode) -> String {
    match serde_json::to_value(AgentPermissionMode::from_runtime(mode)) {
        Ok(Value::String(value)) => value,
        _ => "ask".to_owned(),
    }
}

fn parse_permission_decision(value: &str) -> Result<PermissionDecision, ApiError> {
    match value {
        "allow" => Ok(PermissionDecision::Allow),
        "deny" => Ok(PermissionDecision::Deny),
        "ask" => Ok(PermissionDecision::Ask),
        other => Err(ApiError::bad_request(format!(
            "unknown permission decision `{other}` (expected allow, deny, or ask)"
        ))),
    }
}

fn permission_decision_string(decision: PermissionDecision) -> String {
    match decision {
        PermissionDecision::Allow => "allow",
        PermissionDecision::Deny => "deny",
        PermissionDecision::Ask => "ask",
    }
    .to_owned()
}

fn clean_references(field: &str, values: Vec<String>) -> Result<Vec<String>, ApiError> {
    values
        .into_iter()
        .map(|value| {
            let value = value.trim().to_owned();
            if value.is_empty() {
                Err(ApiError::bad_request(format!(
                    "{field} entries cannot be blank"
                )))
            } else {
                Ok(value)
            }
        })
        .collect()
}

impl TryFrom<AgentMutationInfo> for AgentMutationInput {
    type Error = ApiError;

    fn try_from(info: AgentMutationInfo) -> Result<Self, Self::Error> {
        let name = info.name.trim().to_owned();
        if name.is_empty() {
            return Err(ApiError::bad_request("agent name cannot be blank"));
        }
        let description = info.description.trim().to_owned();
        if description.is_empty() {
            return Err(ApiError::bad_request("agent description cannot be blank"));
        }
        if info.max_turns == 0 {
            return Err(ApiError::bad_request("max turns must be positive"));
        }
        if info.max_tool_rounds == 0 {
            return Err(ApiError::bad_request("max tool rounds must be positive"));
        }
        let mode = parse_permission_mode(&info.permission_mode)?;
        let rules = info
            .permission_rules
            .into_iter()
            .map(|rule| {
                let id = rule.id.trim().to_owned();
                if id.is_empty() {
                    return Err(ApiError::bad_request("permission rule id cannot be blank"));
                }
                let tool = rule.tool.trim().to_owned();
                if tool.is_empty() {
                    return Err(ApiError::bad_request(
                        "permission rule tool cannot be blank",
                    ));
                }
                Ok(PermissionRule {
                    id,
                    tool,
                    decision: parse_permission_decision(&rule.decision)?,
                    command_prefix: rule
                        .command_prefix
                        .map(|value| value.trim().to_owned())
                        .filter(|value| !value.is_empty()),
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(AgentMutationInput {
            name,
            description,
            avatar: AgentAvatar {
                color: info.avatar_color.trim().to_owned(),
            },
            model: info.model.try_into()?,
            permissions: AgentPermissions { mode, rules },
            skills: clean_references("skills", info.skills)?,
            tools: AgentTools {
                native: clean_references("native tools", info.native_tools)?,
                plugins: clean_references("plugin tools", info.plugin_tools)?,
                apps: clean_references("apps", info.apps)?,
            },
            loop_settings: AgentLoop {
                max_turns: info.max_turns,
                max_tool_rounds: info.max_tool_rounds,
            },
        })
    }
}

fn summary_info(
    snapshot: &AgentSnapshot,
    registry: &AgentRegistrySnapshot,
    knowledge_count: u32,
) -> AgentSummaryInfo {
    let profile = &snapshot.profile;
    let tool_count = profile
        .tools
        .native
        .iter()
        .chain(&profile.tools.plugins)
        .chain(&profile.tools.apps)
        .collect::<std::collections::HashSet<_>>()
        .len() as u32;
    AgentSummaryInfo {
        id: profile.id.clone(),
        name: profile.name.clone(),
        description: profile.description.clone(),
        avatar_color: profile.avatar.color.clone(),
        model: profile.model.clone().into(),
        permission_mode: permission_mode_string(profile.permissions.mode),
        skill_count: profile.skills.len() as u32,
        tool_count,
        knowledge_count,
        executable: snapshot.executable,
        validation: snapshot
            .validation
            .iter()
            .map(|issue| AgentValidationInfo {
                field: issue.field.clone(),
                message: issue.message.clone(),
            })
            .collect(),
        is_default: registry.default_agent_id == profile.id,
    }
}

async fn knowledge_count(state: &ApiState, agent_id: &str) -> Result<u32, ApiError> {
    Ok(state
        .agent_knowledge
        .learning_snapshot(agent_id)
        .await?
        .concepts
        .len() as u32)
}

async fn detail_info(
    state: &ApiState,
    snapshot: AgentSnapshot,
    registry: &AgentRegistrySnapshot,
) -> Result<AgentDetailInfo, ApiError> {
    let knowledge = knowledge_count(state, &snapshot.profile.id).await?;
    // Concrete models are enriched with the exact selectable entry whose
    // request value matches the canonical `family/model` name; routes match
    // their selectable named-route entry when one is executable, else None.
    let request_value = match &snapshot.profile.model {
        AgentModel::Concrete { name, .. } => name.clone(),
        AgentModel::Route { route } => route.clone(),
    };
    let model_info = crate::llm_router::client::selectable_native_models(state.cp.store())
        .await?
        .into_iter()
        .find(|candidate| candidate.request_value == request_value);
    let summary = summary_info(&snapshot, registry, knowledge);
    let profile = snapshot.profile;
    let rules = profile
        .permissions
        .rules
        .into_iter()
        .map(|rule| PermissionRuleInfo {
            id: rule.id,
            tool: rule.tool,
            decision: permission_decision_string(rule.decision),
            command_prefix: rule.command_prefix,
        })
        .collect();
    Ok(AgentDetailInfo {
        summary,
        permission_rules: rules,
        skills: profile.skills,
        native_tools: profile.tools.native,
        plugin_tools: profile.tools.plugins,
        apps: profile.tools.apps,
        max_turns: profile.loop_settings.max_turns,
        max_tool_rounds: profile.loop_settings.max_tool_rounds,
        model_info,
    })
}

async fn registry_info(
    state: &ApiState,
    snapshot: AgentRegistrySnapshot,
) -> Result<AgentRegistryInfo, ApiError> {
    let mut agents = Vec::with_capacity(snapshot.agents.len());
    for agent in &snapshot.agents {
        let knowledge = knowledge_count(state, &agent.profile.id).await?;
        agents.push(summary_info(agent, &snapshot, knowledge));
    }
    Ok(AgentRegistryInfo {
        agents,
        default_agent_id: snapshot.default_agent_id,
        recovery: snapshot
            .recovery
            .into_iter()
            .map(|notice| AgentRecoveryInfo {
                code: notice.code,
                message: notice.message,
            })
            .collect(),
        subagent_model: snapshot.subagent_model.into(),
    })
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
        "list_agents" => {
            let snapshot = state.agents.snapshot().await;
            ok(registry_info(state, snapshot).await?)
        }
        "get_agent" => {
            let a: AgentIdP = params(p)?;
            let agent_id = a.agent_id.trim().to_string();
            let registry = state.agents.snapshot().await;
            let snapshot = registry
                .agents
                .iter()
                .find(|agent| agent.profile.id == agent_id)
                .cloned()
                .ok_or_else(|| ApiError::not_found(format!("unknown agent: {agent_id}")))?;
            ok(detail_info(state, snapshot, &registry).await?)
        }
        "create_agent" => {
            let a: CreateAgentP = params(p)?;
            let input: AgentMutationInput = a.input.try_into()?;
            let snapshot = state.agents.create(input).await?;
            let registry = state.agents.snapshot().await;
            ok(detail_info(state, snapshot, &registry).await?)
        }
        "update_agent" => {
            let a: UpdateAgentP = params(p)?;
            let agent_id = a.agent_id.trim().to_string();
            let input: AgentMutationInput = a.input.try_into()?;
            let snapshot = state.agents.update(&agent_id, input).await?;
            let registry = state.agents.snapshot().await;
            ok(detail_info(state, snapshot, &registry).await?)
        }
        "duplicate_agent" => {
            let a: AgentIdP = params(p)?;
            let agent_id = a.agent_id.trim().to_string();
            let snapshot = state.agents.duplicate(&agent_id).await?;
            let registry = state.agents.snapshot().await;
            ok(detail_info(state, snapshot, &registry).await?)
        }
        "delete_agent" => {
            let a: AgentIdP = params(p)?;
            let agent_id = a.agent_id.trim().to_string();
            let snapshot = state.agents.delete(&agent_id).await?;
            ok(registry_info(state, snapshot).await?)
        }
        "set_default_agent" => {
            let a: AgentIdP = params(p)?;
            let agent_id = a.agent_id.trim().to_string();
            let snapshot = state.agents.set_default(&agent_id).await?;
            ok(registry_info(state, snapshot).await?)
        }
        "get_subagent_model" => {
            let snapshot = state.agents.snapshot().await;
            ok(AgentModelInfo::from(snapshot.subagent_model))
        }
        "update_subagent_model" => {
            let a: UpdateSubagentModelP = params(p)?;
            let model: AgentModel = a.model.try_into()?;
            let snapshot = state.agents.set_subagent_model(model).await?;
            ok(registry_info(state, snapshot).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::{
        dispatch,
        tests_support::{state, state_with_agents},
    };
    use crate::domain::{PermMode, Project};
    use serde_json::{json, Value};

    fn reviewer_input(name: &str) -> Value {
        json!({
            "name": name,
            "description": "Reviews implementation quality and regressions.",
            "avatarColor": "violet",
            "model": {"kind":"route","route":"smart"},
            "permissionMode": "ask",
            "permissionRules": [],
            "skills": ["requesting-code-review"],
            "nativeTools": ["read", "grep", "bash"],
            "pluginTools": [],
            "apps": [],
            "maxTurns": 50,
            "maxToolRounds": 100
        })
    }

    #[tokio::test]
    async fn agent_crud_and_duplicate_are_visible_through_rpc() {
        let s = state_with_agents().await;
        let created = dispatch(
            &s,
            "create_agent",
            json!({"input": reviewer_input("Reviewer")}),
        )
        .await
        .unwrap();
        let id = created["summary"]["id"].as_str().unwrap().to_owned();
        assert_eq!(created["summary"]["name"], "Reviewer");

        let updated = dispatch(
            &s,
            "update_agent",
            json!({
                "agent_id": id,
                "input": reviewer_input("Review Lead")
            }),
        )
        .await
        .unwrap();
        assert_eq!(updated["summary"]["name"], "Review Lead");

        let duplicate = dispatch(&s, "duplicate_agent", json!({"agent_id": id}))
            .await
            .unwrap();
        assert_eq!(duplicate["summary"]["name"], "Review Lead Copy");
        assert_ne!(duplicate["summary"]["id"], id);

        let list = dispatch(&s, "list_agents", json!({})).await.unwrap();
        assert!(list["agents"]
            .as_array()
            .unwrap()
            .iter()
            .any(|a| a["id"] == id));
    }

    #[tokio::test]
    async fn get_agent_returns_full_detail() {
        let s = state_with_agents().await;
        let created = dispatch(
            &s,
            "create_agent",
            json!({"input": reviewer_input("Reviewer")}),
        )
        .await
        .unwrap();
        let id = created["summary"]["id"].as_str().unwrap();
        let detail = dispatch(&s, "get_agent", json!({"agent_id": id}))
            .await
            .unwrap();
        assert_eq!(detail["summary"]["id"], *id);
        assert_eq!(detail["summary"]["permissionMode"], "ask");
        assert_eq!(detail["summary"]["skillCount"], 1);
        assert_eq!(detail["summary"]["toolCount"], 3);
        assert_eq!(detail["skills"], json!(["requesting-code-review"]));
        assert_eq!(detail["nativeTools"], json!(["read", "grep", "bash"]));
        assert_eq!(detail["maxTurns"], 50);
        assert_eq!(detail["maxToolRounds"], 100);
        // Route models carry no concrete model metadata.
        assert!(detail["modelInfo"].is_null());
    }

    #[tokio::test]
    async fn delete_rejects_the_final_agent_without_mutation() {
        let s = state_with_agents().await;
        let list = dispatch(&s, "list_agents", json!({})).await.unwrap();
        let id = list["agents"][0]["id"].as_str().unwrap();
        let error = dispatch(&s, "delete_agent", json!({"agent_id": id}))
            .await
            .unwrap_err();
        assert_eq!(error.status, 409);
        assert_eq!(
            dispatch(&s, "list_agents", json!({})).await.unwrap()["agents"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
    }

    #[tokio::test]
    async fn subagent_model_rejects_effort_for_routes() {
        let s = state_with_agents().await;
        let error = dispatch(
            &s,
            "update_subagent_model",
            json!({
                "model": {"kind":"route","route":"fast","effort":"high"}
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(error.status, 400);
    }

    #[tokio::test]
    async fn subagent_model_round_trips_through_rpc() {
        let s = state_with_agents().await;
        let got = dispatch(&s, "get_subagent_model", json!({})).await.unwrap();
        assert_eq!(got, json!({"kind":"route","route":"fast"}));
        let updated = dispatch(
            &s,
            "update_subagent_model",
            json!({"model": {"kind":"route","route":"smart"}}),
        )
        .await
        .unwrap();
        assert_eq!(
            updated["subagentModel"],
            json!({"kind":"route","route":"smart"})
        );
        let got = dispatch(&s, "get_subagent_model", json!({})).await.unwrap();
        assert_eq!(got, json!({"kind":"route","route":"smart"}));
    }

    #[tokio::test]
    async fn case_insensitive_duplicate_name_does_not_change_registry() {
        let s = state_with_agents().await;
        let before = dispatch(&s, "list_agents", json!({})).await.unwrap();
        let name = before["agents"][0]["name"]
            .as_str()
            .unwrap()
            .to_ascii_uppercase();
        let error = dispatch(&s, "create_agent", json!({"input": reviewer_input(&name)}))
            .await
            .unwrap_err();
        assert_eq!(error.status, 400);
        assert_eq!(
            dispatch(&s, "list_agents", json!({})).await.unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn set_default_marks_exactly_one_agent() {
        let s = state_with_agents().await;
        let created = dispatch(
            &s,
            "create_agent",
            json!({"input": reviewer_input("Reviewer")}),
        )
        .await
        .unwrap();
        let id = created["summary"]["id"].as_str().unwrap();
        let registry = dispatch(&s, "set_default_agent", json!({"agent_id": id}))
            .await
            .unwrap();
        assert_eq!(registry["defaultAgentId"], *id);
        assert_eq!(
            registry["agents"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|a| a["isDefault"] == true)
                .count(),
            1
        );
    }

    #[tokio::test]
    async fn mutation_rejects_unknown_permission_mode() {
        let s = state_with_agents().await;
        let mut input = reviewer_input("Reviewer");
        input["permissionMode"] = json!("bypassPermissions");
        let error = dispatch(&s, "create_agent", json!({"input": input}))
            .await
            .unwrap_err();
        assert_eq!(error.status, 400);
    }

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
