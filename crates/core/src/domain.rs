use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum PermMode {
    Default,
    AcceptEdits,
    BypassPermissions,
}

impl PermMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PermMode::Default => "default",
            PermMode::AcceptEdits => "acceptEdits",
            PermMode::BypassPermissions => "bypassPermissions",
        }
    }
    pub fn from_db(s: &str) -> PermMode {
        match s {
            "acceptEdits" => PermMode::AcceptEdits,
            "bypassPermissions" => PermMode::BypassPermissions,
            _ => PermMode::Default,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum SessionStatus {
    Idle,
    Running,
    Interrupted,
    Ended,
}

impl SessionStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            SessionStatus::Idle => "idle",
            SessionStatus::Running => "running",
            SessionStatus::Interrupted => "interrupted",
            SessionStatus::Ended => "ended",
        }
    }
    pub fn from_db(s: &str) -> SessionStatus {
        match s {
            "running" => SessionStatus::Running,
            "interrupted" => SessionStatus::Interrupted,
            "ended" => SessionStatus::Ended,
            _ => SessionStatus::Idle,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Project {
    pub project_id: String,
    pub name: String,
    pub workdir: String,
    pub source: Option<String>,
    pub harness: String,
    pub model: Option<String>,
    pub effort: Option<String>,
    pub perm_mode: PermMode,
    pub created_at: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Session {
    pub session_pk: String,
    pub project_id: String,
    pub agent_session_id: Option<String>,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub title: Option<String>,
    pub status: SessionStatus,
    pub created_at: Option<i64>,
    pub last_active: Option<i64>,
}

/// Internal event emitted by the runtime parser.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AgentEvent {
    Init { session_id: String },
    Status { text: String },
    Text { text: String },
    Result { session_id: Option<String> },
    Error { message: String },
}

/// Public event broadcast to consumers (the Tauri layer re-emits these).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum CoreEvent {
    SessionCreated { session_pk: String, project_id: String },
    Status { session_pk: String, text: String },
    Text { session_pk: String, text: String },
    Result { session_pk: String },
    ApprovalRequested { session_pk: String, request_id: String, tool: String, summary: String },
    Error { session_pk: String, message: String },
    SessionEnded { session_pk: String },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perm_mode_roundtrips_through_db_string() {
        for m in [PermMode::Default, PermMode::AcceptEdits, PermMode::BypassPermissions] {
            assert_eq!(PermMode::from_db(m.as_str()), m);
        }
        assert_eq!(PermMode::from_db("nonsense"), PermMode::Default);
    }

    #[test]
    fn session_status_roundtrips_through_db_string() {
        for s in [SessionStatus::Idle, SessionStatus::Running, SessionStatus::Interrupted, SessionStatus::Ended] {
            assert_eq!(SessionStatus::from_db(s.as_str()), s);
        }
        assert_eq!(SessionStatus::from_db("nonsense"), SessionStatus::Idle);
    }

    #[test]
    fn core_event_serializes_with_kind_tag_and_camel_case() {
        let e = CoreEvent::SessionCreated { session_pk: "s1".into(), project_id: "p1".into() };
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j["kind"], "sessionCreated");
        assert_eq!(j["sessionPk"], "s1");
        assert_eq!(j["projectId"], "p1");
    }
}
