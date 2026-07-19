//! Agent registry commands: thin proxies to the engine daemon's YAML agent
//! registry CRUD, subagent model, learning surface, and selectable-model list
//! shared by agent detail and composer model pickers. Registry commands are
//! runner-aware (`runner_id: Option<String>`, defaulting to `"local"`); the
//! payload helpers below keep the snake_case RPC contract unit-testable
//! without a live engine.

use crate::engine_manager::EngineManager;
use crate::error::CmdError;
use ryuzi_core::llm_router::model_effort::SelectableModelInfo;
use std::sync::Arc;
use tauri::State;

use ryuzi_core::api::types::{
    AgentConfigurationCatalogInfo, AgentDetailInfo, AgentLearningInfo, AgentModelInfo,
    AgentMutationInfo, AgentRegistryInfo, KnowledgeConceptInfo, KnowledgeConceptMutationInfo,
};

// Re-exported by name for a complete, documented DTO surface; specta still
// emits these via the command type graph either way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::{
    AgentRecoveryInfo, AgentSkillUsageInfo, AgentSummaryInfo, AgentValidationInfo,
    CuratorHistorySnapshotInfo, CuratorStateInfo, InvalidKnowledgeConceptInfo,
    JourneyMilestoneInfo, LearningReviewInfo, PermissionRuleInfo,
};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineManager>>;

// --- payload builders (pure; see inline tests) -------------------------------
//
// The engine dispatches on snake_case keys (`agent_api.rs` param structs);
// these builders pin that contract so a rename on either side fails a test
// instead of surfacing as a runtime decode error.

fn agent_id_params(agent_id: &str) -> serde_json::Value {
    serde_json::json!({ "agent_id": agent_id })
}

fn create_agent_params(input: &AgentMutationInfo) -> serde_json::Value {
    serde_json::json!({ "input": input })
}

fn update_agent_params(agent_id: &str, input: &AgentMutationInfo) -> serde_json::Value {
    serde_json::json!({ "agent_id": agent_id, "input": input })
}

fn update_subagent_model_params(model: &AgentModelInfo) -> serde_json::Value {
    serde_json::json!({ "model": model })
}

fn concept_params(agent_id: &str, concept_id: &str) -> serde_json::Value {
    serde_json::json!({ "agent_id": agent_id, "concept_id": concept_id })
}

fn create_concept_params(
    agent_id: &str,
    input: &KnowledgeConceptMutationInfo,
) -> serde_json::Value {
    serde_json::json!({ "agent_id": agent_id, "input": input })
}

fn update_concept_params(
    agent_id: &str,
    concept_id: &str,
    input: &KnowledgeConceptMutationInfo,
) -> serde_json::Value {
    serde_json::json!({ "agent_id": agent_id, "concept_id": concept_id, "input": input })
}

fn raw_concept_params(
    agent_id: &str,
    relative_path: &str,
    raw_markdown: &str,
) -> serde_json::Value {
    serde_json::json!({
        "agent_id": agent_id,
        "relative_path": relative_path,
        "raw_markdown": raw_markdown,
    })
}

fn invalid_concept_params(agent_id: &str, relative_path: &str) -> serde_json::Value {
    serde_json::json!({ "agent_id": agent_id, "relative_path": relative_path })
}

fn rollback_learning_params(agent_id: &str, snapshot_id: &str) -> serde_json::Value {
    serde_json::json!({ "agent_id": agent_id, "snapshot_id": snapshot_id })
}

// --- agent registry commands (runner-aware) ----------------------------------

#[tauri::command]
#[specta::specta]
pub async fn list_agents(engine: Engine<'_>, runner_id: Option<String>) -> R<AgentRegistryInfo> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc("list_agents", serde_json::json!({}))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn get_agent_configuration_catalog(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<AgentConfigurationCatalogInfo> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc("get_agent_configuration_catalog", serde_json::json!({}))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn get_agent(
    engine: Engine<'_>,
    runner_id: Option<String>,
    agent_id: String,
) -> R<AgentDetailInfo> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc("get_agent", agent_id_params(&agent_id))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn create_agent(
    engine: Engine<'_>,
    runner_id: Option<String>,
    input: AgentMutationInfo,
) -> R<AgentDetailInfo> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc("create_agent", create_agent_params(&input))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_agent(
    engine: Engine<'_>,
    runner_id: Option<String>,
    agent_id: String,
    input: AgentMutationInfo,
) -> R<AgentDetailInfo> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc("update_agent", update_agent_params(&agent_id, &input))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn duplicate_agent(
    engine: Engine<'_>,
    runner_id: Option<String>,
    agent_id: String,
) -> R<AgentDetailInfo> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc("duplicate_agent", agent_id_params(&agent_id))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn delete_agent(
    engine: Engine<'_>,
    runner_id: Option<String>,
    agent_id: String,
) -> R<AgentRegistryInfo> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc("delete_agent", agent_id_params(&agent_id))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn set_default_agent(
    engine: Engine<'_>,
    runner_id: Option<String>,
    agent_id: String,
) -> R<AgentRegistryInfo> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc("set_default_agent", agent_id_params(&agent_id))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn get_subagent_model(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<AgentModelInfo> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc("get_subagent_model", serde_json::json!({}))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_subagent_model(
    engine: Engine<'_>,
    runner_id: Option<String>,
    model: AgentModelInfo,
) -> R<AgentRegistryInfo> {
    engine
        .client(runner_id.as_deref().unwrap_or("local"))?
        .rpc(
            "update_subagent_model",
            update_subagent_model_params(&model),
        )
        .await
}

// --- per-agent Learning commands (local-engine-only) --------------------------
//
// Learning data lives in the local engine's knowledge trees, so these never
// take a `runner_id`.

#[tauri::command]
#[specta::specta]
pub async fn get_agent_learning(engine: Engine<'_>, agent_id: String) -> R<AgentLearningInfo> {
    engine
        .client("local")?
        .rpc("get_agent_learning", agent_id_params(&agent_id))
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn create_agent_concept(
    engine: Engine<'_>,
    agent_id: String,
    input: KnowledgeConceptMutationInfo,
) -> R<KnowledgeConceptInfo> {
    engine
        .client("local")?
        .rpc(
            "create_agent_concept",
            create_concept_params(&agent_id, &input),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn update_agent_concept(
    engine: Engine<'_>,
    agent_id: String,
    concept_id: String,
    input: KnowledgeConceptMutationInfo,
) -> R<KnowledgeConceptInfo> {
    engine
        .client("local")?
        .rpc(
            "update_agent_concept",
            update_concept_params(&agent_id, &concept_id, &input),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn delete_agent_concept(
    engine: Engine<'_>,
    agent_id: String,
    concept_id: String,
) -> R<AgentLearningInfo> {
    engine
        .client("local")?
        .rpc(
            "delete_agent_concept",
            concept_params(&agent_id, &concept_id),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn validate_agent_concept_raw(
    engine: Engine<'_>,
    agent_id: String,
    relative_path: String,
    raw_markdown: String,
) -> R<KnowledgeConceptInfo> {
    engine
        .client("local")?
        .rpc(
            "validate_agent_concept_raw",
            raw_concept_params(&agent_id, &relative_path, &raw_markdown),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn replace_agent_concept_raw(
    engine: Engine<'_>,
    agent_id: String,
    relative_path: String,
    raw_markdown: String,
) -> R<KnowledgeConceptInfo> {
    engine
        .client("local")?
        .rpc(
            "replace_agent_concept_raw",
            raw_concept_params(&agent_id, &relative_path, &raw_markdown),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn delete_invalid_agent_concept(
    engine: Engine<'_>,
    agent_id: String,
    relative_path: String,
) -> R<AgentLearningInfo> {
    engine
        .client("local")?
        .rpc(
            "delete_invalid_agent_concept",
            invalid_concept_params(&agent_id, &relative_path),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn rollback_agent_learning(
    engine: Engine<'_>,
    agent_id: String,
    snapshot_id: String,
) -> R<AgentLearningInfo> {
    engine
        .client("local")?
        .rpc(
            "rollback_agent_learning",
            rollback_learning_params(&agent_id, &snapshot_id),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn list_selectable_models(
    engine: Engine<'_>,
    runner_id: Option<String>,
) -> R<Vec<SelectableModelInfo>> {
    let client = engine.client(runner_id.as_deref().unwrap_or("local"))?;
    client
        .rpc("list_selectable_models", serde_json::json!({}))
        .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryuzi_core::api::types::{AgentModelInfo, AgentPersonalityInfo};

    fn fixture_mutation() -> AgentMutationInfo {
        AgentMutationInfo {
            name: "Reviewer".to_owned(),
            description: "Reviews implementation quality and regressions.".to_owned(),
            avatar_color: "violet".to_owned(),
            model: AgentModelInfo::Route {
                route: "free".to_owned(),
            },
            personality: AgentPersonalityInfo {
                preset: "helpful".to_owned(),
                custom: None,
            },
            permission_mode: "ask".to_owned(),
            permission_rules: Vec::new(),
            skills: vec!["requesting-code-review".to_owned()],
            native_tools: vec!["read".to_owned(), "grep".to_owned(), "bash".to_owned()],
            plugin_tools: Vec::new(),
            apps: Vec::new(),
        }
    }

    fn fixture_concept_mutation() -> KnowledgeConceptMutationInfo {
        KnowledgeConceptMutationInfo {
            title: "Prefer Store::with_conn".to_owned(),
            description: "DB access goes through the shared pool helper.".to_owned(),
            body: "Never hand-roll pool boilerplate at call sites.".to_owned(),
            scope: "global".to_owned(),
            project_id: None,
            tags: vec!["store".to_owned()],
        }
    }

    #[test]
    fn agent_id_payload_matches_core_rpc_contract() {
        assert_eq!(
            agent_id_params("reviewer"),
            serde_json::json!({"agent_id": "reviewer"})
        );
    }

    #[test]
    fn create_agent_payload_matches_core_rpc_contract() {
        let input = fixture_mutation();
        assert_eq!(
            create_agent_params(&input),
            serde_json::json!({ "input": input })
        );
    }

    #[test]
    fn update_agent_payload_matches_core_rpc_contract() {
        let input = fixture_mutation();
        assert_eq!(
            update_agent_params("reviewer", &input),
            serde_json::json!({"agent_id": "reviewer", "input": input})
        );
    }

    #[test]
    fn update_subagent_model_payload_matches_core_rpc_contract() {
        let model = AgentModelInfo::Concrete {
            name: "claude-sonnet-4-5".to_owned(),
            effort: Some("high".to_owned()),
        };
        assert_eq!(
            update_subagent_model_params(&model),
            serde_json::json!({ "model": model })
        );
    }

    #[test]
    fn learning_payload_always_carries_agent_id() {
        assert_eq!(
            concept_params("reviewer", "concept-1"),
            serde_json::json!({"agent_id": "reviewer", "concept_id": "concept-1"})
        );
    }

    #[test]
    fn create_concept_payload_matches_core_rpc_contract() {
        let input = fixture_concept_mutation();
        assert_eq!(
            create_concept_params("reviewer", &input),
            serde_json::json!({"agent_id": "reviewer", "input": input})
        );
    }

    #[test]
    fn update_concept_payload_matches_core_rpc_contract() {
        let input = fixture_concept_mutation();
        assert_eq!(
            update_concept_params("reviewer", "concept-1", &input),
            serde_json::json!({
                "agent_id": "reviewer",
                "concept_id": "concept-1",
                "input": input
            })
        );
    }

    #[test]
    fn raw_concept_payload_matches_core_rpc_contract() {
        assert_eq!(
            raw_concept_params("reviewer", "memory/global/store.md", "# Store"),
            serde_json::json!({
                "agent_id": "reviewer",
                "relative_path": "memory/global/store.md",
                "raw_markdown": "# Store"
            })
        );
    }

    #[test]
    fn invalid_concept_payload_matches_core_rpc_contract() {
        assert_eq!(
            invalid_concept_params("reviewer", "memory/global/broken.md"),
            serde_json::json!({
                "agent_id": "reviewer",
                "relative_path": "memory/global/broken.md"
            })
        );
    }

    #[test]
    fn rollback_payload_matches_core_rpc_contract() {
        assert_eq!(
            rollback_learning_params("reviewer", "snap-1"),
            serde_json::json!({"agent_id": "reviewer", "snapshot_id": "snap-1"})
        );
    }
}
