use indexmap::IndexMap;
use serde::{Deserialize, Serialize};
use serde_yaml::Value;

use crate::agents::personality::AgentPersonality;
use crate::PermMode;

pub type AgentId = String;
pub const AGENT_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq)]
pub struct AgentIndex {
    pub schema_version: u32,
    pub order: Vec<AgentId>,
    pub default_agent_id: AgentId,
    pub extensions: IndexMap<String, Value>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentModel {
    Concrete {
        name: String,
        effort: Option<String>,
    },
    Route {
        route: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentAvatar {
    pub color: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    Allow,
    Deny,
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum AgentPermissionMode {
    Ask,
    AcceptEdits,
    Full,
    Plan,
}

impl AgentPermissionMode {
    pub fn runtime_mode(self) -> PermMode {
        match self {
            Self::Ask => PermMode::Default,
            Self::AcceptEdits => PermMode::AcceptEdits,
            Self::Full => PermMode::BypassPermissions,
            Self::Plan => PermMode::Plan,
        }
    }

    pub fn from_runtime(mode: PermMode) -> Self {
        match mode {
            PermMode::Default => Self::Ask,
            PermMode::AcceptEdits => Self::AcceptEdits,
            PermMode::BypassPermissions => Self::Full,
            PermMode::Plan => Self::Plan,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PermissionRule {
    pub id: String,
    pub tool: String,
    pub decision: PermissionDecision,
    pub command_prefix: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentPermissions {
    pub mode: PermMode,
    pub rules: Vec<PermissionRule>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentTools {
    pub native: Vec<String>,
    pub plugins: Vec<String>,
    pub apps: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentLoop {
    pub max_turns: u32,
    pub max_tool_rounds: u32,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentProfile {
    pub schema_version: u32,
    pub id: AgentId,
    pub name: String,
    pub description: String,
    pub avatar: AgentAvatar,
    pub model: AgentModel,
    pub personality: AgentPersonality,
    pub permissions: AgentPermissions,
    pub skills: Vec<String>,
    pub tools: AgentTools,
    pub loop_settings: AgentLoop,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SubagentConfig {
    pub schema_version: u32,
    pub model: AgentModel,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentValidationIssue {
    pub field: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentSnapshot {
    pub profile: AgentProfile,
    pub executable: bool,
    pub validation: Vec<AgentValidationIssue>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentRecoveryNotice {
    pub code: String,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentRegistrySnapshot {
    pub agents: Vec<AgentSnapshot>,
    pub default_agent_id: AgentId,
    pub recovery: Vec<AgentRecoveryNotice>,
    pub subagent_model: AgentModel,
}

#[derive(Debug, Clone, PartialEq)]
pub struct AgentMutationInput {
    pub name: String,
    pub description: String,
    pub avatar: AgentAvatar,
    pub model: AgentModel,
    pub personality: AgentPersonality,
    pub permissions: AgentPermissions,
    pub skills: Vec<String>,
    pub tools: AgentTools,
    pub loop_settings: AgentLoop,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegistryDiskImage {
    pub index_yaml: String,
    pub subagents_yaml: String,
    pub agents: IndexMap<AgentId, String>,
    pub deleted_agent_ids: Vec<AgentId>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct RegistryCandidate {
    pub index: AgentIndex,
    pub profiles: IndexMap<AgentId, AgentProfile>,
    pub subagents: SubagentConfig,
    pub recovery: Vec<AgentRecoveryNotice>,
}
