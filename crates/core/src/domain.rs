use serde::{Deserialize, Serialize};
use specta::Type;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub enum PermMode {
    Default,
    AcceptEdits,
    BypassPermissions,
    Plan,
}

impl PermMode {
    pub fn as_str(&self) -> &'static str {
        match self {
            PermMode::Default => "default",
            PermMode::AcceptEdits => "acceptEdits",
            PermMode::BypassPermissions => "bypassPermissions",
            PermMode::Plan => "plan",
        }
    }
    pub fn from_db(s: &str) -> PermMode {
        match s {
            "acceptEdits" => PermMode::AcceptEdits,
            "bypassPermissions" => PermMode::BypassPermissions,
            "plan" => PermMode::Plan,
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
    pub started_by: Option<String>,
    pub created_at: Option<i64>,
    pub last_active: Option<i64>,
    pub resume_attempts: i64,
}

/// An MCP server the agent can use as tools (attached to an ACP session in Spec 3).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct McpServerSpec {
    pub name: String,
    pub transport: McpTransport,
}

/// How to reach an MCP server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum McpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
}

/// Where a session is driven from (a gateway channel + conversation).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Surface {
    pub gateway: String,
    pub conversation_id: String,
}

/// Who initiated an action, across gateways.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Actor {
    pub id: String,
    pub gateway: String,
}

/// A file a user attached to a message, before it has been downloaded.
/// Mirrors `packages/protocol/src/index.ts`'s `AttachmentRef` (not part of
/// the specta/Tauri type export surface — this crosses gateway boundaries,
/// not the cockpit IPC boundary).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentRef {
    pub name: String,
    pub url: String,
    pub content_type: Option<String>,
    pub size: u64,
}

/// A tool-approval request surfaced to a gateway / UI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovalRequest {
    pub request_id: String,
    pub tool: String,
    pub summary: String,
    /// Role ids allowed to approve, beyond the session starter. Empty means
    /// "starter only" (see `policy::can_approve`).
    #[serde(default)]
    pub approver_role_ids: Vec<String>,
    /// Actor id that started the session, for starter-always approval.
    #[serde(default)]
    pub started_by: Option<String>,
    /// Optional approval timeout, in milliseconds.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

/// The user's decision on a tool-approval request. Mirrors ACP permission kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ApprovalDecision {
    AllowOnce,
    AllowAlways,
    RejectOnce,
    RejectAlways,
    Cancel,
}

/// A persisted transcript entry. Forward-compatible with ACP session/update blocks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct Message {
    pub session_pk: String,
    pub seq: i64,
    pub role: String,       // user | assistant | system
    pub block_type: String, // text | thought | tool_call | plan | status | error
    pub payload: serde_json::Value,
    pub tool_call_id: Option<String>,
    pub status: Option<String>,
    pub tool_kind: Option<String>,
    pub created_at: i64,
}

/// Input to `Store::insert_message`; `seq` and `created_at` are assigned by the store.
#[derive(Debug, Clone, PartialEq)]
pub struct NewMessage {
    pub session_pk: String,
    pub role: String,
    pub block_type: String,
    pub payload: serde_json::Value,
    pub tool_call_id: Option<String>,
    pub status: Option<String>,
    pub tool_kind: Option<String>,
}

impl NewMessage {
    /// Convenience for a simple text/status/error block.
    pub fn block(
        session_pk: &str,
        role: &str,
        block_type: &str,
        payload: serde_json::Value,
    ) -> Self {
        NewMessage {
            session_pk: session_pk.to_string(),
            role: role.to_string(),
            block_type: block_type.to_string(),
            payload,
            tool_call_id: None,
            status: None,
            tool_kind: None,
        }
    }
}

/// Public event broadcast to consumers (the Tauri layer re-emits these).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Type)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum CoreEvent {
    SessionCreated {
        session_pk: String,
        project_id: String,
    },
    Message {
        session_pk: String,
        seq: i64,
        role: String,
        block_type: String,
        payload: serde_json::Value,
        tool_call_id: Option<String>,
        status: Option<String>,
        tool_kind: Option<String>,
    },
    Result {
        session_pk: String,
    },
    ApprovalRequested {
        session_pk: String,
        request_id: String,
        tool: String,
        summary: String,
    },
    Error {
        session_pk: String,
        message: String,
    },
    /// Out-of-band announcement (e.g. "update available") rendered to every
    /// surface of a session. TS parity: `{ kind: "notice", session_pk, text }`.
    Notice {
        session_pk: String,
        text: String,
    },
    SessionEnded {
        session_pk: String,
    },
    /// A scheduled job run started or finished (status: running|success|failed).
    JobRunChanged {
        job_id: String,
        run_id: String,
        status: String,
    },
    /// A runtime npm install/update produced an output line.
    RuntimeUpdateLog {
        runtime_id: String,
        line: String,
    },
    /// A runtime npm install/update finished (ok=false → message has detail).
    RuntimeUpdateDone {
        runtime_id: String,
        ok: bool,
        message: Option<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perm_mode_roundtrips_through_db_string() {
        for m in [
            PermMode::Default,
            PermMode::AcceptEdits,
            PermMode::BypassPermissions,
            PermMode::Plan,
        ] {
            assert_eq!(PermMode::from_db(m.as_str()), m);
        }
        assert_eq!(PermMode::from_db("nonsense"), PermMode::Default);
    }

    #[test]
    fn session_status_roundtrips_through_db_string() {
        for s in [
            SessionStatus::Idle,
            SessionStatus::Running,
            SessionStatus::Interrupted,
            SessionStatus::Ended,
        ] {
            assert_eq!(SessionStatus::from_db(s.as_str()), s);
        }
        assert_eq!(SessionStatus::from_db("nonsense"), SessionStatus::Idle);
    }

    #[test]
    fn mcp_server_spec_round_trips_through_json() {
        let spec = McpServerSpec {
            name: "notion".into(),
            transport: McpTransport::Stdio {
                command: "notion-mcp".into(),
                args: vec!["--stdio".into()],
                env: vec![("TOKEN".into(), "x".into())],
            },
        };
        let j = serde_json::to_string(&spec).unwrap();
        let back: McpServerSpec = serde_json::from_str(&j).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn core_event_serializes_with_camel_tag_and_snake_fields() {
        let e = CoreEvent::SessionCreated {
            session_pk: "s1".into(),
            project_id: "p1".into(),
        };
        let j = serde_json::to_value(&e).unwrap();
        assert_eq!(j["kind"], "sessionCreated");
        assert_eq!(j["session_pk"], "s1");
        assert_eq!(j["project_id"], "p1");
    }
}
