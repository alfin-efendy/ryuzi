//! Agent registry RPC family: YAML-backed CRUD, duplicate/default selection,
//! per-agent capabilities and learning, the shared subagent model, and the
//! selectable-model list consumed by agent detail and composer model pickers.

use super::{ok, params, ApiError};
use crate::agents::knowledge::AgentLearningSnapshot;
use crate::agents::learning_queue::{LearningEventPayload, RollbackEvent};
use crate::agents::okf::{ConceptArea, KnowledgeConcept, KnowledgeConceptInput, KnowledgeScope};
use crate::agents::types::{
    AgentAvatar, AgentLoop, AgentModel, AgentMutationInput, AgentPermissionMode, AgentPermissions,
    AgentRegistrySnapshot, AgentSnapshot, AgentTools, PermissionDecision, PermissionRule,
};
use crate::api::types::{
    AgentDetailInfo, AgentLearningInfo, AgentModelInfo, AgentMutationInfo, AgentRecoveryInfo,
    AgentRegistryInfo, AgentSkillUsageInfo, AgentSummaryInfo, AgentValidationInfo,
    CuratorHistorySnapshotInfo, CuratorStateInfo, InvalidKnowledgeConceptInfo,
    JourneyMilestoneInfo, KnowledgeConceptInfo, KnowledgeConceptMutationInfo, LearningReviewInfo,
    PermissionRuleInfo, SessionRuntimeInfo,
};
use crate::llm_router::model_effort::{
    self, EffectiveEffortSource, ModelDefaultSource, ModelPreferenceKey, ProjectRuntimeInfo,
    StoredEffortStatus,
};
use crate::serve::ApiState;
use crate::{PermMode, SessionKind};
use chrono::SecondsFormat;
use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
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
    "get_agent_learning",
    "create_agent_concept",
    "update_agent_concept",
    "delete_agent_concept",
    "validate_agent_concept_raw",
    "replace_agent_concept_raw",
    "delete_invalid_agent_concept",
    "rollback_agent_learning",
];

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

#[derive(Deserialize)]
struct ConceptP {
    agent_id: String,
    concept_id: String,
}

#[derive(Deserialize)]
struct CreateConceptP {
    agent_id: String,
    input: KnowledgeConceptMutationInfo,
}

#[derive(Deserialize)]
struct UpdateConceptP {
    agent_id: String,
    concept_id: String,
    input: KnowledgeConceptMutationInfo,
}

#[derive(Deserialize)]
struct RawConceptP {
    agent_id: String,
    relative_path: String,
    raw_markdown: String,
}

#[derive(Deserialize)]
struct InvalidConceptP {
    agent_id: String,
    relative_path: String,
}

#[derive(Deserialize)]
struct RollbackLearningP {
    agent_id: String,
    snapshot_id: String,
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum PostCommitEnrichmentFailure {
    Knowledge,
    Models,
}

#[cfg(test)]
tokio::task_local! {
    static POST_COMMIT_ENRICHMENT_FAILURE: PostCommitEnrichmentFailure;
}

#[cfg(test)]
async fn with_post_commit_enrichment_failure<T>(
    failure: PostCommitEnrichmentFailure,
    future: impl std::future::Future<Output = T>,
) -> T {
    POST_COMMIT_ENRICHMENT_FAILURE.scope(failure, future).await
}

fn injected_post_commit_enrichment_failure(failure: PostCommitEnrichmentFailure) -> bool {
    #[cfg(test)]
    {
        POST_COMMIT_ENRICHMENT_FAILURE
            .try_with(|active| *active == failure)
            .unwrap_or(false)
    }
    #[cfg(not(test))]
    {
        let _ = failure;
        false
    }
}

async fn knowledge_count(state: &ApiState, agent_id: &str) -> Result<u32, ApiError> {
    if injected_post_commit_enrichment_failure(PostCommitEnrichmentFailure::Knowledge) {
        return Err(anyhow::anyhow!("injected knowledge enrichment failure").into());
    }
    Ok(state
        .agent_knowledge
        .learning_snapshot(agent_id)
        .await?
        .concepts
        .len() as u32)
}

async fn agent_model_info(
    state: &ApiState,
    model: &AgentModel,
) -> Result<Option<crate::llm_router::model_effort::SelectableModelInfo>, ApiError> {
    let AgentModel::Concrete { name, .. } = model else {
        return Ok(None);
    };
    if injected_post_commit_enrichment_failure(PostCommitEnrichmentFailure::Models) {
        return Err(anyhow::anyhow!("injected model enrichment failure").into());
    }
    Ok(
        crate::llm_router::client::selectable_native_models(state.cp.store())
            .await?
            .into_iter()
            .find(|candidate| candidate.request_value == *name),
    )
}

async fn detail_info(
    state: &ApiState,
    snapshot: AgentSnapshot,
    registry: &AgentRegistrySnapshot,
) -> Result<AgentDetailInfo, ApiError> {
    let knowledge = knowledge_count(state, &snapshot.profile.id).await?;
    let model_info = agent_model_info(state, &snapshot.profile.model).await?;
    Ok(detail_info_from_enrichment(
        snapshot, registry, knowledge, model_info,
    ))
}

async fn post_commit_detail_info(
    state: &ApiState,
    snapshot: AgentSnapshot,
    registry: &AgentRegistrySnapshot,
) -> AgentDetailInfo {
    let agent_id = snapshot.profile.id.as_str();
    let knowledge = match knowledge_count(state, agent_id).await {
        Ok(count) => count,
        Err(error) => {
            tracing::warn!(
                ?error,
                agent_id,
                enrichment = "knowledge",
                "agent mutation committed but response enrichment failed"
            );
            0
        }
    };
    let model_info = match agent_model_info(state, &snapshot.profile.model).await {
        Ok(model_info) => model_info,
        Err(error) => {
            tracing::warn!(
                ?error,
                agent_id,
                enrichment = "model",
                "agent mutation committed but response enrichment failed"
            );
            None
        }
    };
    detail_info_from_enrichment(snapshot, registry, knowledge, model_info)
}

fn detail_info_from_enrichment(
    snapshot: AgentSnapshot,
    registry: &AgentRegistrySnapshot,
    knowledge: u32,
    model_info: Option<crate::llm_router::model_effort::SelectableModelInfo>,
) -> AgentDetailInfo {
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
    AgentDetailInfo {
        summary,
        permission_rules: rules,
        skills: profile.skills,
        native_tools: profile.tools.native,
        plugin_tools: profile.tools.plugins,
        apps: profile.tools.apps,
        max_turns: profile.loop_settings.max_turns,
        max_tool_rounds: profile.loop_settings.max_tool_rounds,
        model_info,
    }
}

fn registry_info_from_counts(
    snapshot: AgentRegistrySnapshot,
    knowledge_counts: &std::collections::HashMap<String, u32>,
) -> AgentRegistryInfo {
    let agents = snapshot
        .agents
        .iter()
        .map(|agent| {
            summary_info(
                agent,
                &snapshot,
                knowledge_counts
                    .get(&agent.profile.id)
                    .copied()
                    .unwrap_or_default(),
            )
        })
        .collect();
    AgentRegistryInfo {
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
    }
}

async fn registry_info(
    state: &ApiState,
    snapshot: AgentRegistrySnapshot,
) -> Result<AgentRegistryInfo, ApiError> {
    let mut counts = std::collections::HashMap::with_capacity(snapshot.agents.len());
    for agent in &snapshot.agents {
        counts.insert(
            agent.profile.id.clone(),
            knowledge_count(state, &agent.profile.id).await?,
        );
    }
    Ok(registry_info_from_counts(snapshot, &counts))
}

async fn post_commit_registry_info(
    state: &ApiState,
    snapshot: AgentRegistrySnapshot,
) -> AgentRegistryInfo {
    let mut counts = std::collections::HashMap::with_capacity(snapshot.agents.len());
    for agent in &snapshot.agents {
        match knowledge_count(state, &agent.profile.id).await {
            Ok(count) => {
                counts.insert(agent.profile.id.clone(), count);
            }
            Err(error) => {
                tracing::warn!(
                    ?error,
                    agent_id = agent.profile.id,
                    enrichment = "knowledge",
                    "agent mutation committed but response enrichment failed"
                );
            }
        }
    }
    registry_info_from_counts(snapshot, &counts)
}

fn clean_optional(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn effective_model_default(
    model_info: Option<&model_effort::SelectableModelInfo>,
) -> (Option<String>, EffectiveEffortSource) {
    let Some(info) = model_info else {
        return (None, EffectiveEffortSource::None);
    };
    let source = match info.default_source {
        ModelDefaultSource::Configured => EffectiveEffortSource::Configured,
        ModelDefaultSource::Provider => EffectiveEffortSource::Provider,
        ModelDefaultSource::VariesByTarget | ModelDefaultSource::None => {
            EffectiveEffortSource::None
        }
    };
    (info.resolved_default.clone(), source)
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
    let route_target_default = match (project.model.as_deref(), model_info.as_ref()) {
        (Some(model), Some(info)) => {
            crate::llm_router::client::named_route_target_default(
                state.cp.store(),
                model,
                &info.supported,
            )
            .await?
        }
        _ => None,
    };
    let is_named_route = model_info
        .as_ref()
        .is_some_and(|info| info.kind == model_effort::SelectableModelKind::NamedRoute);
    let (effective_effort, effective_source) = if is_named_route {
        route_target_default
            .map(|effort| (Some(effort), EffectiveEffortSource::RouteTarget))
            .unwrap_or_else(|| effective_model_default(model_info.as_ref()))
    } else if let Some(effort) = project_effort {
        (Some(effort.clone()), EffectiveEffortSource::Project)
    } else if let Some(effort) = route_target_default {
        (Some(effort), EffectiveEffortSource::RouteTarget)
    } else {
        effective_model_default(model_info.as_ref())
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
    let route_target_default = match (runtime.model.as_deref(), model_info.as_ref()) {
        (Some(model), Some(info)) => {
            crate::llm_router::client::named_route_target_default(
                state.cp.store(),
                model,
                &info.supported,
            )
            .await?
        }
        _ => None,
    };
    let is_named_route = model_info
        .as_ref()
        .is_some_and(|info| info.kind == model_effort::SelectableModelKind::NamedRoute);
    let (effective_effort, effective_source) = if is_named_route {
        route_target_default
            .map(|effort| (Some(effort), EffectiveEffortSource::RouteTarget))
            .unwrap_or_else(|| effective_model_default(model_info.as_ref()))
    } else if let Some(effort) = explicit {
        (Some(effort.clone()), EffectiveEffortSource::Session)
    } else if let Some(effort) = route_target_default {
        (Some(effort), EffectiveEffortSource::RouteTarget)
    } else {
        effective_model_default(model_info.as_ref())
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

/// Confirms the agent exists (404 otherwise) and returns its trimmed id.
/// Every per-agent Learning handler goes through this first so deleted or
/// unknown agents can never touch a knowledge bundle path.
async fn require_agent(state: &ApiState, agent_id: &str) -> Result<String, ApiError> {
    let agent_id = agent_id.trim().to_owned();
    state
        .agents
        .get(&agent_id)
        .await
        .map_err(|_| ApiError::not_found(format!("unknown agent: {agent_id}")))?;
    Ok(agent_id)
}

fn knowledge_store(
    state: &ApiState,
    agent_id: &str,
) -> Result<crate::agents::knowledge::KnowledgeStore, ApiError> {
    state
        .agent_knowledge
        .for_agent(agent_id)
        .map_err(knowledge_api_error)
}

fn require_memory_path(relative_path: &str) -> Result<(), ApiError> {
    if !is_memory_relative_path(relative_path) {
        return Err(ApiError::not_found("memory concept was not found"));
    }
    Ok(())
}

async fn require_invalid_memory_path(
    knowledge: &crate::agents::knowledge::KnowledgeStore,
    relative_path: &str,
) -> Result<(), ApiError> {
    require_memory_path(relative_path)?;
    let scan = knowledge.scan().await.map_err(knowledge_api_error)?;
    if !scan
        .invalid
        .iter()
        .any(|invalid| invalid.relative_path == relative_path)
    {
        return Err(ApiError::not_found("invalid memory concept was not found"));
    }
    Ok(())
}

fn is_memory_relative_path(relative_path: &str) -> bool {
    relative_path.starts_with("memory/global/")
        || relative_path.starts_with("memory/user/")
        || relative_path.starts_with("memory/projects/")
}

/// Knowledge-store failures may include host filesystem paths in their error
/// chains. Log the full diagnostic server-side, but return only stable,
/// path-free messages to callers. Missing resources remain 404s; validation
/// and mutation failures use the existing 400 contract.
fn knowledge_api_error(error: anyhow::Error) -> ApiError {
    let diagnostic = format!("{error:#}");
    tracing::warn!(error = %diagnostic, "agent knowledge operation failed");
    if diagnostic.contains("was not found") || diagnostic.contains("does not exist") {
        ApiError::not_found("knowledge resource was not found")
    } else {
        ApiError::bad_request("knowledge operation failed")
    }
}

/// The wire scope vocabulary is exactly `global`, `user`, and `project`;
/// `project_id` must be nonblank for `project` and absent for the others.
/// This management surface authors memory concepts only, so `extensions`
/// is always empty — existing extension fields survive updates because the
/// store merges through its lossless document.
fn concept_input_from_mutation(
    info: KnowledgeConceptMutationInfo,
) -> Result<KnowledgeConceptInput, ApiError> {
    let title = info.title.trim().to_owned();
    if title.is_empty() {
        return Err(ApiError::bad_request("concept title cannot be blank"));
    }
    let description = info.description.trim().to_owned();
    if description.is_empty() {
        return Err(ApiError::bad_request("concept description cannot be blank"));
    }
    let project_id = info
        .project_id
        .map(|value| value.trim().to_owned())
        .filter(|value| !value.is_empty());
    let scope = match info.scope.trim() {
        "project" => KnowledgeScope::Project {
            project_id: project_id.ok_or_else(|| {
                ApiError::bad_request("scope `project` requires a nonblank projectId")
            })?,
        },
        scope @ ("global" | "user") => {
            if project_id.is_some() {
                return Err(ApiError::bad_request(format!(
                    "scope `{scope}` does not take a projectId"
                )));
            }
            if scope == "global" {
                KnowledgeScope::Global
            } else {
                KnowledgeScope::User
            }
        }
        other => {
            return Err(ApiError::bad_request(format!(
                "unknown scope `{other}` (expected global, user, or project)"
            )));
        }
    };
    Ok(KnowledgeConceptInput {
        area: ConceptArea::Memory(scope),
        title,
        description,
        body: info.body,
        tags: info.tags,
        extensions: IndexMap::new(),
    })
}

fn concept_info(concept: KnowledgeConcept) -> KnowledgeConceptInfo {
    let (scope, project_id) = match &concept.scope {
        None => (None, None),
        Some(KnowledgeScope::Global) => (Some("global"), None),
        Some(KnowledgeScope::User) => (Some("user"), None),
        Some(KnowledgeScope::Project { project_id }) => (Some("project"), Some(project_id.clone())),
    };
    KnowledgeConceptInfo {
        id: concept.id,
        relative_path: concept.relative_path,
        concept_type: concept.concept_type,
        title: concept.title,
        description: concept.description,
        body: concept.body,
        scope: scope.map(str::to_owned),
        project_id,
        tags: concept.tags,
        timestamp: concept.timestamp.to_rfc3339_opts(SecondsFormat::Secs, true),
    }
}

fn learning_info(snapshot: AgentLearningSnapshot) -> AgentLearningInfo {
    AgentLearningInfo {
        concepts: snapshot.concepts.into_iter().map(concept_info).collect(),
        invalid: snapshot
            .invalid
            .into_iter()
            .map(|invalid| InvalidKnowledgeConceptInfo {
                relative_path: invalid.relative_path,
                error: invalid.error,
                raw_markdown: invalid.raw_markdown,
            })
            .collect(),
        journey: snapshot
            .journey
            .into_iter()
            .map(|milestone| JourneyMilestoneInfo {
                concept_id: milestone.concept_id,
                title: milestone.title,
                timestamp: milestone
                    .timestamp
                    .to_rfc3339_opts(SecondsFormat::Secs, true),
            })
            .collect(),
        skill_usage: snapshot
            .skill_usage
            .into_iter()
            .map(|usage| AgentSkillUsageInfo {
                skill_id: usage.skill_id,
                uses: usage.uses,
                successes: usage.successes,
                concept_id: usage.concept_id,
            })
            .collect(),
        reviews: snapshot
            .reviews
            .into_iter()
            .map(|review| LearningReviewInfo {
                concept_id: review.concept_id,
                title: review.title,
                description: review.description,
                timestamp: review.timestamp.to_rfc3339_opts(SecondsFormat::Secs, true),
            })
            .collect(),
        curator: CuratorStateInfo {
            concept: snapshot.curator.concept.map(concept_info),
            last_event_id: snapshot.curator.last_event_id,
        },
        curator_history: snapshot
            .curator_history
            .into_iter()
            .map(|history| CuratorHistorySnapshotInfo {
                snapshot_id: history.snapshot_id,
                concept: concept_info(history.concept),
            })
            .collect(),
    }
}

async fn agent_learning_info(
    state: &ApiState,
    agent_id: &str,
) -> Result<AgentLearningInfo, ApiError> {
    Ok(learning_info(
        state
            .agent_knowledge
            .learning_snapshot(agent_id)
            .await
            .map_err(knowledge_api_error)?,
    ))
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
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
            ok(post_commit_detail_info(state, snapshot, &registry).await)
        }
        "update_agent" => {
            let a: UpdateAgentP = params(p)?;
            let agent_id = a.agent_id.trim().to_string();
            let input: AgentMutationInput = a.input.try_into()?;
            let snapshot = state.agents.update(&agent_id, input).await?;
            let registry = state.agents.snapshot().await;
            ok(post_commit_detail_info(state, snapshot, &registry).await)
        }
        "duplicate_agent" => {
            let a: AgentIdP = params(p)?;
            let agent_id = a.agent_id.trim().to_string();
            let snapshot = state.agents.duplicate(&agent_id).await?;
            let registry = state.agents.snapshot().await;
            ok(post_commit_detail_info(state, snapshot, &registry).await)
        }
        "delete_agent" => {
            let a: AgentIdP = params(p)?;
            let agent_id = a.agent_id.trim().to_string();
            let snapshot = state.agents.delete(&agent_id).await?;
            ok(post_commit_registry_info(state, snapshot).await)
        }
        "set_default_agent" => {
            let a: AgentIdP = params(p)?;
            let agent_id = a.agent_id.trim().to_string();
            let snapshot = state.agents.set_default(&agent_id).await?;
            ok(post_commit_registry_info(state, snapshot).await)
        }
        "get_subagent_model" => {
            let snapshot = state.agents.snapshot().await;
            ok(AgentModelInfo::from(snapshot.subagent_model))
        }
        "update_subagent_model" => {
            let a: UpdateSubagentModelP = params(p)?;
            let model: AgentModel = a.model.try_into()?;
            let snapshot = state.agents.set_subagent_model(model).await?;
            ok(post_commit_registry_info(state, snapshot).await)
        }
        "get_agent_learning" => {
            let a: AgentIdP = params(p)?;
            let agent_id = require_agent(state, &a.agent_id).await?;
            ok(agent_learning_info(state, &agent_id).await?)
        }
        "create_agent_concept" => {
            let a: CreateConceptP = params(p)?;
            let agent_id = require_agent(state, &a.agent_id).await?;
            let input = concept_input_from_mutation(a.input)?;
            let knowledge = knowledge_store(state, &agent_id)?;
            let concept = knowledge.create(input).await.map_err(knowledge_api_error)?;
            ok(concept_info(concept))
        }
        "update_agent_concept" => {
            let a: UpdateConceptP = params(p)?;
            let agent_id = require_agent(state, &a.agent_id).await?;
            let input = concept_input_from_mutation(a.input)?;
            let knowledge = knowledge_store(state, &agent_id)?;
            let concept = knowledge
                .update_memory(&a.concept_id, input)
                .await
                .map_err(knowledge_api_error)?;
            ok(concept_info(concept))
        }
        "delete_agent_concept" => {
            let a: ConceptP = params(p)?;
            let agent_id = require_agent(state, &a.agent_id).await?;
            let knowledge = knowledge_store(state, &agent_id)?;
            knowledge
                .delete_memory(&a.concept_id)
                .await
                .map_err(knowledge_api_error)?;
            ok(agent_learning_info(state, &agent_id).await?)
        }
        "validate_agent_concept_raw" => {
            let a: RawConceptP = params(p)?;
            let agent_id = require_agent(state, &a.agent_id).await?;
            let knowledge = knowledge_store(state, &agent_id)?;
            require_invalid_memory_path(&knowledge, &a.relative_path).await?;
            let concept = knowledge
                .validate_raw(&a.relative_path, &a.raw_markdown)
                .await
                .map_err(knowledge_api_error)?;
            ok(concept_info(concept))
        }
        "replace_agent_concept_raw" => {
            let a: RawConceptP = params(p)?;
            let agent_id = require_agent(state, &a.agent_id).await?;
            let knowledge = knowledge_store(state, &agent_id)?;
            require_invalid_memory_path(&knowledge, &a.relative_path).await?;
            let concept = knowledge
                .replace_invalid_raw(&a.relative_path, &a.raw_markdown)
                .await
                .map_err(knowledge_api_error)?;
            ok(concept_info(concept))
        }
        "delete_invalid_agent_concept" => {
            let a: InvalidConceptP = params(p)?;
            let agent_id = require_agent(state, &a.agent_id).await?;
            let knowledge = knowledge_store(state, &agent_id)?;
            require_invalid_memory_path(&knowledge, &a.relative_path).await?;
            knowledge
                .delete_invalid(&a.relative_path)
                .await
                .map_err(knowledge_api_error)?;
            ok(agent_learning_info(state, &agent_id).await?)
        }
        "rollback_agent_learning" => {
            let a: RollbackLearningP = params(p)?;
            let agent_id = require_agent(state, &a.agent_id).await?;
            let snapshot_id = a.snapshot_id.trim().to_owned();
            // Reject unknown snapshots before enqueueing: a rollback event
            // that can never apply would otherwise sit at the head of the
            // durable queue and block every later Learning event.
            let snapshot = state
                .agent_knowledge
                .learning_snapshot(&agent_id)
                .await
                .map_err(knowledge_api_error)?;
            if !snapshot
                .curator_history
                .iter()
                .any(|history| history.snapshot_id == snapshot_id)
            {
                return Err(ApiError::not_found(format!(
                    "rollback snapshot `{snapshot_id}` was not found"
                )));
            }
            state
                .learning_queue
                .deliver_through(
                    &agent_id,
                    LearningEventPayload::Rollback(RollbackEvent {
                        snapshot_id,
                        restored_concept_ids: Vec::new(),
                    }),
                )
                .await
                .map_err(knowledge_api_error)?;
            ok(agent_learning_info(state, &agent_id).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::agents::yaml::parse_agent_profile_document;
    use crate::api::{
        dispatch,
        tests_support::{state, state_with_agents},
    };
    use crate::domain::{PermMode, Project};
    use crate::llm_router::connections;
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
    async fn update_agent_preserves_profile_extensions_through_rpc() {
        let mut s = state_with_agents().await;
        let agent_id = s.agents.default_agent_id().await;
        let profile_path = s
            .agent_knowledge
            .for_agent(&agent_id)
            .unwrap()
            .root()
            .parent()
            .unwrap()
            .join("agent.yaml");
        let mut seeded = std::fs::read_to_string(&profile_path).unwrap();
        seeded.push_str("x-vendor:\n  review_profile: strict\n");
        std::fs::write(&profile_path, seeded).unwrap();
        let config_root = profile_path.ancestors().nth(3).unwrap().to_path_buf();
        s.agents = std::sync::Arc::new(
            crate::agents::registry::AgentRegistry::load(config_root, s.cp.store().clone())
                .await
                .unwrap(),
        );

        let mut input = reviewer_input("Ryuzi");
        input["description"] = json!("Reviews safety and regressions.");
        let updated = dispatch(
            &s,
            "update_agent",
            json!({"agent_id": agent_id, "input": input}),
        )
        .await
        .unwrap();

        let raw = std::fs::read_to_string(profile_path).unwrap();
        let persisted = parse_agent_profile_document(&raw).unwrap();
        assert_eq!(
            persisted.extensions()["x-vendor"]["review_profile"],
            "strict"
        );
        assert_eq!(
            updated["summary"]["description"],
            "Reviews safety and regressions."
        );
    }

    #[tokio::test]
    async fn post_commit_knowledge_failures_do_not_invert_agent_mutations() {
        let s = state_with_agents().await;

        let created = super::with_post_commit_enrichment_failure(
            super::PostCommitEnrichmentFailure::Knowledge,
            dispatch(
                &s,
                "create_agent",
                json!({"input": reviewer_input("Reviewer")}),
            ),
        )
        .await
        .unwrap();
        let id = created["summary"]["id"].as_str().unwrap().to_owned();
        assert_eq!(created["summary"]["knowledgeCount"], 0);

        let updated = super::with_post_commit_enrichment_failure(
            super::PostCommitEnrichmentFailure::Knowledge,
            dispatch(
                &s,
                "update_agent",
                json!({"agent_id": id, "input": reviewer_input("Review Lead")}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(updated["summary"]["name"], "Review Lead");

        let duplicate = super::with_post_commit_enrichment_failure(
            super::PostCommitEnrichmentFailure::Knowledge,
            dispatch(&s, "duplicate_agent", json!({"agent_id": id})),
        )
        .await
        .unwrap();
        let duplicate_id = duplicate["summary"]["id"].as_str().unwrap().to_owned();

        let defaulted = super::with_post_commit_enrichment_failure(
            super::PostCommitEnrichmentFailure::Knowledge,
            dispatch(&s, "set_default_agent", json!({"agent_id": id})),
        )
        .await
        .unwrap();
        assert_eq!(defaulted["defaultAgentId"], id);

        let subagent = super::with_post_commit_enrichment_failure(
            super::PostCommitEnrichmentFailure::Knowledge,
            dispatch(
                &s,
                "update_subagent_model",
                json!({"model": {"kind": "route", "route": "smart"}}),
            ),
        )
        .await
        .unwrap();
        assert_eq!(subagent["subagentModel"]["route"], "smart");

        let deleted = super::with_post_commit_enrichment_failure(
            super::PostCommitEnrichmentFailure::Knowledge,
            dispatch(&s, "delete_agent", json!({"agent_id": duplicate_id})),
        )
        .await
        .unwrap();
        assert!(deleted["agents"]
            .as_array()
            .unwrap()
            .iter()
            .all(|agent| agent["id"] != duplicate_id));

        let authoritative = s.agents.snapshot().await;
        assert_eq!(authoritative.default_agent_id, id);
        assert!(authoritative
            .agents
            .iter()
            .any(|agent| agent.profile.id == id && agent.profile.name == "Review Lead"));
        assert!(authoritative
            .agents
            .iter()
            .all(|agent| agent.profile.id != duplicate_id));
        assert!(matches!(
            authoritative.subagent_model,
            crate::agents::types::AgentModel::Route { ref route } if route == "smart"
        ));
    }

    #[tokio::test]
    async fn post_commit_model_failures_degrade_concrete_mutation_details() {
        use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};

        let s = state_with_agents().await;
        connections::add_connection(
            s.cp.store(),
            ConnectionRow {
                id: "anthropic-live".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Anthropic".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    api_key: Some("test-key".into()),
                    models_override: Some(vec!["claude-opus-4-8".into()]),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        let mut input = reviewer_input("Reviewer");
        input["model"] =
            json!({"kind": "concrete", "name": "anthropic/claude-opus-4-8", "effort": null});

        let created = super::with_post_commit_enrichment_failure(
            super::PostCommitEnrichmentFailure::Models,
            dispatch(&s, "create_agent", json!({"input": input})),
        )
        .await
        .unwrap();
        let id = created["summary"]["id"].as_str().unwrap().to_owned();
        assert!(created["modelInfo"].is_null());

        let mut update = reviewer_input("Review Lead");
        update["model"] =
            json!({"kind": "concrete", "name": "anthropic/claude-opus-4-8", "effort": null});
        let updated = super::with_post_commit_enrichment_failure(
            super::PostCommitEnrichmentFailure::Models,
            dispatch(&s, "update_agent", json!({"agent_id": id, "input": update})),
        )
        .await
        .unwrap();
        assert!(updated["modelInfo"].is_null());

        let duplicate = super::with_post_commit_enrichment_failure(
            super::PostCommitEnrichmentFailure::Models,
            dispatch(&s, "duplicate_agent", json!({"agent_id": id})),
        )
        .await
        .unwrap();
        assert!(duplicate["modelInfo"].is_null());

        let authoritative = dispatch(&s, "get_agent", json!({"agent_id": id}))
            .await
            .unwrap();
        assert_eq!(authoritative["summary"]["name"], "Review Lead");
        assert_eq!(
            authoritative["modelInfo"]["requestValue"],
            "anthropic/claude-opus-4-8"
        );
    }

    #[tokio::test]
    async fn route_detail_never_exposes_resolver_metadata() {
        use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};

        let s = state_with_agents().await;
        connections::add_connection(
            s.cp.store(),
            ConnectionRow {
                id: "anthropic-route-target".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Anthropic".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    api_key: Some("test-key".into()),
                    models_override: Some(vec!["claude-opus-4-8".into()]),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();

        let id = s.agents.default_agent_id().await;
        let detail = dispatch(&s, "get_agent", json!({"agent_id": id}))
            .await
            .unwrap();
        assert_eq!(detail["summary"]["model"]["kind"], "route");
        assert!(detail["modelInfo"].is_null());
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
    async fn default_routes_allow_agent_and_subagent_model_changes() {
        let s = state().await;
        connections::add_connection(
            s.cp.store(),
            connections::ConnectionRow {
                id: "anthropic-live".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Anthropic".into(),
                priority: 0,
                enabled: true,
                data: connections::ConnectionData {
                    api_key: Some("test-key".into()),
                    models_override: Some(vec!["claude-opus-4-8".into()]),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        let agent_id = s.agents.default_agent_id().await;

        let updated = dispatch(
            &s,
            "update_agent",
            json!({"agent_id": agent_id, "input": reviewer_input("Ryuzi")}),
        )
        .await
        .unwrap();
        assert_eq!(
            updated["summary"]["model"],
            json!({"kind":"route","route":"smart"})
        );

        let subagents = dispatch(
            &s,
            "update_subagent_model",
            json!({"model": {"kind":"route","route":"smart"}}),
        )
        .await
        .unwrap();
        assert_eq!(
            subagents["subagentModel"],
            json!({"kind":"route","route":"smart"})
        );
    }

    async fn default_agent_id(s: &crate::serve::ApiState) -> String {
        s.agents.default_agent_id().await
    }

    /// A `state_with_agents()` whose default agent has one unparseable
    /// Markdown file inside its knowledge bundle's `memory/user` area.
    async fn state_with_invalid_concept() -> crate::serve::ApiState {
        let s = state_with_agents().await;
        let id = default_agent_id(&s).await;
        let root = s
            .agent_knowledge
            .for_agent(&id)
            .unwrap()
            .root()
            .to_path_buf();
        std::fs::create_dir_all(root.join("memory/user")).unwrap();
        std::fs::write(root.join("memory/user/broken.md"), "nope").unwrap();
        s
    }

    fn valid_memory_markdown(agent_id: &str) -> String {
        format!(
            "---\ntype: Memory\ntitle: Repaired\ndescription: Repaired memory.\n\
             timestamp: 2026-07-12T14:30:00Z\nscope: user\nagent_id: {agent_id}\n---\n\n\
             Repaired body.\n"
        )
    }

    fn user_concept_input() -> Value {
        json!({
            "title": "Concise summaries",
            "description": "Prefer concise summaries.",
            "body": "Identify changed files and checks.",
            "scope": "user",
            "projectId": null,
            "tags": ["communication"]
        })
    }

    #[tokio::test]
    async fn learning_exposes_and_mutates_only_memory_owned_concepts() {
        let s = state_with_agents().await;
        let id = default_agent_id(&s).await;
        let knowledge = s.agent_knowledge.for_agent(&id).unwrap();
        knowledge
            .replace_raw(
                "learning/journey/internal.md",
                "---\ntype: Journey\ntitle: Internal milestone\ndescription: Must remain owned by Learning.\ntimestamp: 2026-08-01T00:00:00Z\n---\n\nInternal body.\n",
            )
            .await
            .unwrap();

        let learning = dispatch(&s, "get_agent_learning", json!({"agent_id": id}))
            .await
            .unwrap();
        assert!(learning["concepts"].as_array().unwrap().is_empty());
        assert_eq!(learning["journey"][0]["title"], "Internal milestone");

        for method in ["update_agent_concept", "delete_agent_concept"] {
            let params = if method == "update_agent_concept" {
                json!({"agent_id": id, "concept_id": "internal", "input": user_concept_input()})
            } else {
                json!({"agent_id": id, "concept_id": "internal"})
            };
            let error = dispatch(&s, method, params).await.unwrap_err();
            assert_eq!(error.status, 404);
        }
        for method in [
            "validate_agent_concept_raw",
            "replace_agent_concept_raw",
            "delete_invalid_agent_concept",
        ] {
            let params = if method == "delete_invalid_agent_concept" {
                json!({"agent_id": id, "relative_path": "learning/journey/internal.md"})
            } else {
                json!({
                    "agent_id": id,
                    "relative_path": "learning/journey/internal.md",
                    "raw_markdown": "---\ntype: Journey\ntitle: Replaced\ndescription: Must remain owned by Learning.\ntimestamp: 2026-08-01T00:00:00Z\n---\n\nReplacement.\n"
                })
            };
            let error = dispatch(&s, method, params).await.unwrap_err();
            assert_eq!(
                error.status, 404,
                "{method} must reject Learning-owned paths"
            );
        }

        knowledge
            .replace_raw(
                "memory/user/owned.md",
                "---\ntype: Memory\ntitle: Owned memory\ndescription: Must use memory CRUD.\ntimestamp: 2026-08-01T00:00:00Z\nscope: user\n---\n\nOwned body.\n",
            )
            .await
            .unwrap();
        for method in ["replace_agent_concept_raw", "delete_invalid_agent_concept"] {
            let params = if method == "delete_invalid_agent_concept" {
                json!({"agent_id": id, "relative_path": "memory/user/owned.md"})
            } else {
                json!({
                    "agent_id": id,
                    "relative_path": "memory/user/owned.md",
                    "raw_markdown": "---\ntype: Memory\ntitle: Replaced\ndescription: Must use memory CRUD.\ntimestamp: 2026-08-01T00:00:00Z\nscope: user\n---\n\nReplacement.\n"
                })
            };
            let error = dispatch(&s, method, params).await.unwrap_err();
            assert_eq!(error.status, 404, "{method} must reject valid memory files");
        }

        let preserved = knowledge.read("internal").await.unwrap();
        assert_eq!(preserved.concept_type, "Journey");
        assert_eq!(preserved.relative_path, "learning/journey/internal.md");
    }

    #[tokio::test]
    async fn curator_history_is_newest_first_with_snapshot_id_tie_break() {
        let s = state_with_agents().await;
        let id = default_agent_id(&s).await;
        let knowledge = s.agent_knowledge.for_agent(&id).unwrap();
        for (snapshot_id, timestamp) in [
            ("z-old", "2026-01-01T00:00:00Z"),
            ("a-new", "2026-12-01T00:00:00Z"),
            ("z-new", "2026-12-01T00:00:00Z"),
        ] {
            knowledge
                .replace_raw(
                    &format!("curator/history/{snapshot_id}.md"),
                    &format!("---\ntype: CuratorHistory\ntitle: {snapshot_id}\ndescription: Snapshot.\ntimestamp: {timestamp}\n---\n\nBody.\n"),
                )
                .await
                .unwrap();
        }

        let learning = dispatch(&s, "get_agent_learning", json!({"agent_id": id}))
            .await
            .unwrap();
        let ids = learning["curatorHistory"]
            .as_array()
            .unwrap()
            .iter()
            .map(|item| item["snapshotId"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(ids, ["z-new", "a-new", "z-old"]);
    }

    #[tokio::test]
    async fn knowledge_errors_do_not_expose_absolute_paths() {
        for leaked in [
            "/home/alice/.config/ryuzi/agents/reviewer/knowledge/.knowledge-transactions/tx-1",
            r"C:\Users\Alice\AppData\Roaming\ryuzi\agents\reviewer\knowledge\.knowledge-transactions\tx-1",
        ] {
            let error =
                super::knowledge_api_error(anyhow::anyhow!("transaction failed at {leaked}"));
            assert!(!error.message.contains(leaked));
            assert!(!error.message.contains("/home/alice"));
            assert!(!error.message.contains(r"C:\Users\Alice"));
        }
    }

    #[tokio::test]
    async fn knowledge_crud_is_scoped_by_agent_id() {
        let s = state_with_agents().await;
        let a = dispatch(&s, "list_agents", json!({})).await.unwrap()["agents"][0]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        let b = dispatch(
            &s,
            "create_agent",
            json!({"input": reviewer_input("Reviewer")}),
        )
        .await
        .unwrap()["summary"]["id"]
            .as_str()
            .unwrap()
            .to_owned();
        dispatch(
            &s,
            "create_agent_concept",
            json!({"agent_id": a, "input": user_concept_input()}),
        )
        .await
        .unwrap();
        assert_eq!(
            dispatch(&s, "get_agent_learning", json!({"agent_id": a}))
                .await
                .unwrap()["concepts"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            dispatch(&s, "get_agent_learning", json!({"agent_id": b}))
                .await
                .unwrap()["concepts"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn raw_repair_validation_does_not_write_until_replace() {
        let s = state_with_invalid_concept().await;
        let id = default_agent_id(&s).await;
        let raw = valid_memory_markdown(&id);
        dispatch(
            &s,
            "validate_agent_concept_raw",
            json!({
                "agent_id": id, "relative_path": "memory/user/broken.md", "raw_markdown": raw
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            dispatch(&s, "get_agent_learning", json!({"agent_id": id}))
                .await
                .unwrap()["invalid"]
                .as_array()
                .unwrap()
                .len(),
            1
        );
        let unparseable = dispatch(
            &s,
            "validate_agent_concept_raw",
            json!({
                "agent_id": id, "relative_path": "memory/user/broken.md",
                "raw_markdown": "still broken"
            }),
        )
        .await
        .unwrap_err();
        assert_eq!(unparseable.status, 400);
        dispatch(
            &s,
            "replace_agent_concept_raw",
            json!({
                "agent_id": id, "relative_path": "memory/user/broken.md", "raw_markdown": raw
            }),
        )
        .await
        .unwrap();
        assert_eq!(
            dispatch(&s, "get_agent_learning", json!({"agent_id": id}))
                .await
                .unwrap()["invalid"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn raw_validate_and_replace_reject_blank_descriptions() {
        for method in ["validate_agent_concept_raw", "replace_agent_concept_raw"] {
            for description in ["", "   "] {
                let s = state_with_invalid_concept().await;
                let id = default_agent_id(&s).await;
                let raw = format!(
                    "---\ntype: Memory\ntitle: Repaired\ndescription: '{description}'\ntimestamp: 2026-07-12T14:30:00Z\nscope: user\nagent_id: {id}\n---\n\nBody.\n"
                );
                let error = dispatch(
                    &s,
                    method,
                    json!({
                        "agent_id": id,
                        "relative_path": "memory/user/broken.md",
                        "raw_markdown": raw,
                    }),
                )
                .await
                .unwrap_err();
                assert_eq!(error.status, 400, "{method} accepted `{description}`");
                assert_eq!(
                    dispatch(&s, "get_agent_learning", json!({"agent_id": id}))
                        .await
                        .unwrap()["invalid"]
                        .as_array()
                        .unwrap()
                        .len(),
                    1,
                    "{method} changed the invalid file"
                );
            }
        }
    }

    #[tokio::test]
    async fn concept_mutations_round_trip_scope_project_and_rfc3339_timestamp() {
        let s = state_with_agents().await;
        let id = default_agent_id(&s).await;
        let created = dispatch(
            &s,
            "create_agent_concept",
            json!({"agent_id": id, "input": {
                "title": "Project fact", "description": "Project memory.", "body": "Body.",
                "scope": "project", "projectId": "p1", "tags": ["ops"]
            }}),
        )
        .await
        .unwrap();
        assert_eq!(created["scope"], "project");
        assert_eq!(created["projectId"], "p1");
        assert_eq!(created["conceptType"], "Memory");
        assert!(created["relativePath"]
            .as_str()
            .unwrap()
            .starts_with("memory/projects/p1/"));
        assert_eq!(created["tags"], json!(["ops"]));
        chrono::DateTime::parse_from_rfc3339(created["timestamp"].as_str().unwrap()).unwrap();

        let concept_id = created["id"].as_str().unwrap().to_owned();
        let updated = dispatch(
            &s,
            "update_agent_concept",
            json!({"agent_id": id, "concept_id": concept_id, "input": {
                "title": "Global fact", "description": "Global memory.", "body": "Body 2.",
                "scope": "global", "projectId": null, "tags": []
            }}),
        )
        .await
        .unwrap();
        assert_eq!(updated["scope"], "global");
        assert!(updated["projectId"].is_null());
        assert!(updated["relativePath"]
            .as_str()
            .unwrap()
            .starts_with("memory/global/"));

        dispatch(
            &s,
            "delete_agent_concept",
            json!({"agent_id": id, "concept_id": concept_id}),
        )
        .await
        .unwrap();
        assert_eq!(
            dispatch(&s, "get_agent_learning", json!({"agent_id": id}))
                .await
                .unwrap()["concepts"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn learning_handlers_enforce_agent_404_and_scope_400() {
        let s = state_with_agents().await;
        let id = default_agent_id(&s).await;
        for (method, params) in [
            ("get_agent_learning", json!({"agent_id": "missing"})),
            (
                "create_agent_concept",
                json!({"agent_id": "missing", "input": user_concept_input()}),
            ),
            (
                "delete_invalid_agent_concept",
                json!({"agent_id": "missing", "relative_path": "memory/user/x.md"}),
            ),
            (
                "rollback_agent_learning",
                json!({"agent_id": "missing", "snapshot_id": "snap"}),
            ),
        ] {
            let error = dispatch(&s, method, params).await.unwrap_err();
            assert_eq!(error.status, 404, "{method} should 404 for unknown agents");
        }

        let missing_project = dispatch(
            &s,
            "create_agent_concept",
            json!({"agent_id": id, "input": {
                "title": "T", "description": "D", "body": "B",
                "scope": "project", "projectId": null, "tags": []
            }}),
        )
        .await
        .unwrap_err();
        assert_eq!(missing_project.status, 400);

        let stray_project = dispatch(
            &s,
            "create_agent_concept",
            json!({"agent_id": id, "input": {
                "title": "T", "description": "D", "body": "B",
                "scope": "user", "projectId": "p1", "tags": []
            }}),
        )
        .await
        .unwrap_err();
        assert_eq!(stray_project.status, 400);

        let unknown_scope = dispatch(
            &s,
            "create_agent_concept",
            json!({"agent_id": id, "input": {
                "title": "T", "description": "D", "body": "B",
                "scope": "workspace", "projectId": null, "tags": []
            }}),
        )
        .await
        .unwrap_err();
        assert_eq!(unknown_scope.status, 400);

        let unknown_concept = dispatch(
            &s,
            "update_agent_concept",
            json!({"agent_id": id, "concept_id": "nope", "input": user_concept_input()}),
        )
        .await
        .unwrap_err();
        assert_eq!(unknown_concept.status, 404);
    }

    #[tokio::test]
    async fn delete_invalid_agent_concept_removes_the_broken_file() {
        let s = state_with_invalid_concept().await;
        let id = default_agent_id(&s).await;
        dispatch(
            &s,
            "delete_invalid_agent_concept",
            json!({"agent_id": id, "relative_path": "memory/user/broken.md"}),
        )
        .await
        .unwrap();
        assert_eq!(
            dispatch(&s, "get_agent_learning", json!({"agent_id": id}))
                .await
                .unwrap()["invalid"]
                .as_array()
                .unwrap()
                .len(),
            0
        );
    }

    #[tokio::test]
    async fn rollback_agent_learning_restores_snapshot_through_the_durable_queue() {
        use crate::agents::learning_queue::{CuratorStateEvent, LearningEventPayload};
        let s = state_with_agents().await;
        let id = default_agent_id(&s).await;
        s.learning_queue
            .deliver_through(
                &id,
                LearningEventPayload::CuratorState(CuratorStateEvent {
                    title: "Current state".into(),
                    description: "Current description.".into(),
                    body: "Current body.".into(),
                }),
            )
            .await
            .unwrap();
        let knowledge = s.agent_knowledge.for_agent(&id).unwrap();
        knowledge
            .replace_raw(
                "curator/history/snap1.md",
                "---\ntype: CuratorHistory\ntitle: Snapshotted state\n\
                 description: State before curation.\ntimestamp: 2026-07-12T14:30:00Z\n---\n\n\
                 Snapshot body.\n",
            )
            .await
            .unwrap();

        let unknown = dispatch(
            &s,
            "rollback_agent_learning",
            json!({"agent_id": id, "snapshot_id": "missing"}),
        )
        .await
        .unwrap_err();
        assert_eq!(unknown.status, 404);

        let out = dispatch(
            &s,
            "rollback_agent_learning",
            json!({"agent_id": id, "snapshot_id": "snap1"}),
        )
        .await
        .unwrap();
        assert_eq!(out["curator"]["concept"]["title"], "Snapshotted state");
        assert!(out["curatorHistory"].as_array().unwrap().len() >= 2);
        // The rollback drained through the queue: nothing is left pending.
        assert!(s
            .learning_queue
            .claim_next(&id, "assert")
            .await
            .unwrap()
            .is_none());
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
