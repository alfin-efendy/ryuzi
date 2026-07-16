use anyhow::anyhow;

use crate::agents::registry::AgentRegistry;
use crate::domain::AgentIdentitySnapshot;
use crate::store::Store;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAgentAccess {
    Executable { agent_id: String },
    LegacyReadOnly,
    DeletedReadOnly { snapshot: AgentIdentitySnapshot },
}

pub async fn resolve_session_agent_access(
    store: &Store,
    registry: &AgentRegistry,
    session_pk: &str,
) -> anyhow::Result<SessionAgentAccess> {
    let session = store
        .get_session(session_pk)
        .await?
        .ok_or_else(|| anyhow!("session `{session_pk}` was not found"))?;
    match (session.primary_agent_id, session.primary_agent_snapshot) {
        (None, None) => Ok(SessionAgentAccess::LegacyReadOnly),
        (Some(agent_id), Some(snapshot)) => {
            let exists = registry
                .snapshot()
                .await
                .agents
                .iter()
                .any(|agent| agent.profile.id == agent_id);
            if !exists {
                return Ok(SessionAgentAccess::DeletedReadOnly { snapshot });
            }
            registry.get(&agent_id).await?;
            Ok(SessionAgentAccess::Executable { agent_id })
        }
        _ => Err(anyhow!(
            "session `{session_pk}` has corrupt primary agent ownership"
        )),
    }
}

/// A typed failure for the centralized session-access guard. Modeled on
/// [`crate::mentions::MentionError`] (`Clone` + `std::error::Error`) so it can
/// ride an `anyhow::Error` up through a `ControlPlane` result and still be
/// recovered by `downcast_ref` at the API boundary into the right HTTP status.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionAccessError {
    /// The session's agent history is read-only (legacy or deleted owner). The
    /// carried string is the exact, user-facing conflict message.
    ReadOnly(String),
    /// Resolving the session's agent access failed (unknown or corrupt
    /// session). The carried string is the underlying error's message.
    Resolve(String),
}

impl std::fmt::Display for SessionAccessError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::ReadOnly(message) | Self::Resolve(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for SessionAccessError {}

impl From<anyhow::Error> for SessionAccessError {
    fn from(error: anyhow::Error) -> Self {
        SessionAccessError::Resolve(error.to_string())
    }
}

/// The single guard every run/mutation entry point funnels through: it returns
/// the session's executable primary-agent id, or a typed [`SessionAccessError`]
/// that maps to a 409 conflict (read-only) at the API boundary. Historical
/// sessions — legacy (no owner) and deleted-owner alike — are refused here, so
/// no call site special-cases `primary_agent_id.is_none()`.
pub async fn require_executable_session_agent(
    store: &Store,
    registry: &AgentRegistry,
    session_pk: &str,
) -> Result<String, SessionAccessError> {
    match resolve_session_agent_access(store, registry, session_pk).await? {
        SessionAgentAccess::Executable { agent_id } => Ok(agent_id),
        SessionAgentAccess::LegacyReadOnly => Err(SessionAccessError::ReadOnly(
            "Legacy agent history is read-only.".into(),
        )),
        SessionAgentAccess::DeletedReadOnly { snapshot } => Err(SessionAccessError::ReadOnly(
            format!("{} was deleted; this history is read-only.", snapshot.name),
        )),
    }
}

#[cfg(test)]
mod tests {
    use std::path::Path;
    use std::sync::Arc;

    use super::*;
    use crate::agents::bootstrap::{ensure_default_routes, initialize_agent_registry};
    use crate::agents::registry::AgentRegistry;
    use crate::domain::{PermMode, Session, SessionKind, SessionStatus};
    use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};

    fn plan6_contract(
        session: Session,
        snapshot: AgentIdentitySnapshot,
        access: SessionAgentAccess,
    ) {
        let _: Option<String> = session.primary_agent_id;
        let _: Option<AgentIdentitySnapshot> = session.primary_agent_snapshot;
        let _ = snapshot;
        match access {
            SessionAgentAccess::Executable { agent_id } => drop(agent_id),
            SessionAgentAccess::LegacyReadOnly => {}
            SessionAgentAccess::DeletedReadOnly { snapshot } => drop(snapshot),
        }
    }

    #[test]
    fn plan6_ownership_contract_type_checks() {
        let _: fn(Session, AgentIdentitySnapshot, SessionAgentAccess) = plan6_contract;
    }

    fn session(
        session_pk: &str,
        primary_agent_id: Option<&str>,
        primary_agent_snapshot: Option<AgentIdentitySnapshot>,
    ) -> Session {
        Session {
            session_pk: session_pk.into(),
            primary_agent_id: primary_agent_id.map(str::to_string),
            primary_agent_snapshot,
            project_id: None,
            agent_session_id: None,
            worktree_path: None,
            branch: None,
            title: None,
            status: SessionStatus::Idle,
            perm_mode: PermMode::Default,
            started_by: None,
            created_at: None,
            last_active: None,
            resume_attempts: 0,
            branch_owned: false,
            kind: SessionKind::Chat,
            speaker: None,
            agent: None,
            parent_session_pk: None,
        }
    }

    fn snapshot(id: &str) -> AgentIdentitySnapshot {
        AgentIdentitySnapshot {
            id: id.into(),
            name: "Ada at creation".into(),
            avatar_color: "violet".into(),
        }
    }

    async fn initialized_registry(root: &Path, store: Arc<Store>) -> Arc<AgentRegistry> {
        connections::add_connection(
            &store,
            ConnectionRow {
                id: "anthropic-live".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Anthropic".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    models_override: Some(vec!["claude-opus-4-8".into()]),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        ensure_default_routes(&store).await.unwrap();
        initialize_agent_registry(root.to_owned(), store.clone())
            .await
            .unwrap();
        std::fs::write(
            root.join("agents/subagents.yaml"),
            "schema_version: 1\nmodel: { name: anthropic/claude-opus-4-8, effort: high }\n",
        )
        .unwrap();
        Arc::new(AgentRegistry::load(root.to_owned(), store).await.unwrap())
    }

    #[tokio::test]
    async fn resolve_session_agent_access_handles_executable_legacy_deleted_corrupt_and_missing_sessions(
    ) {
        let root = tempfile::tempdir().unwrap();
        let db_path = root.path().join("store.sqlite");
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        let registry = initialized_registry(root.path(), store.clone()).await;

        store
            .insert_session(session(
                "executable",
                Some("ryuzi"),
                Some(snapshot("ryuzi")),
            ))
            .await
            .unwrap();
        store
            .insert_session(session("legacy", None, None))
            .await
            .unwrap();
        store
            .insert_session(session(
                "deleted",
                Some("removed"),
                Some(snapshot("removed")),
            ))
            .await
            .unwrap();
        store
            .insert_session(session("corrupt", Some("ryuzi"), None))
            .await
            .unwrap();

        assert_eq!(
            resolve_session_agent_access(&store, &registry, "executable")
                .await
                .unwrap(),
            SessionAgentAccess::Executable {
                agent_id: "ryuzi".into()
            }
        );
        assert_eq!(
            resolve_session_agent_access(&store, &registry, "legacy")
                .await
                .unwrap(),
            SessionAgentAccess::LegacyReadOnly
        );
        assert_eq!(
            resolve_session_agent_access(&store, &registry, "deleted")
                .await
                .unwrap(),
            SessionAgentAccess::DeletedReadOnly {
                snapshot: snapshot("removed")
            }
        );
        assert!(resolve_session_agent_access(&store, &registry, "corrupt")
            .await
            .unwrap_err()
            .to_string()
            .contains("corrupt"));
        assert!(resolve_session_agent_access(&store, &registry, "missing")
            .await
            .unwrap_err()
            .to_string()
            .contains("not found"));
    }

    #[tokio::test]
    async fn resolve_session_agent_access_keeps_the_persisted_snapshot_after_a_registry_rename() {
        let root = tempfile::tempdir().unwrap();
        let db_path = root.path().join("store.sqlite");
        let store = Arc::new(Store::open(&db_path).await.unwrap());
        let registry = initialized_registry(root.path(), store.clone()).await;
        let persisted = snapshot("ryuzi");
        store
            .insert_session(session("owned", Some("ryuzi"), Some(persisted.clone())))
            .await
            .unwrap();

        registry
            .update(
                "ryuzi",
                crate::agents::types::AgentMutationInput {
                    name: "Renamed Ryuzi".into(),
                    description: "Updated".into(),
                    avatar: crate::agents::types::AgentAvatar {
                        color: "blue".into(),
                    },
                    model: crate::agents::types::AgentModel::Concrete {
                        name: "anthropic/claude-opus-4-8".into(),
                        effort: Some("high".into()),
                    },
                    permissions: crate::agents::types::AgentPermissions {
                        mode: PermMode::Default,
                        rules: Vec::new(),
                    },
                    skills: Vec::new(),
                    tools: crate::agents::types::AgentTools {
                        native: vec!["read".into()],
                        plugins: Vec::new(),
                        apps: Vec::new(),
                    },
                    loop_settings: crate::agents::types::AgentLoop {
                        max_turns: 10,
                        max_tool_rounds: 20,
                    },
                },
            )
            .await
            .unwrap();

        assert_eq!(
            resolve_session_agent_access(&store, &registry, "owned")
                .await
                .unwrap(),
            SessionAgentAccess::Executable {
                agent_id: "ryuzi".into()
            }
        );
        assert_eq!(
            store
                .get_session("owned")
                .await
                .unwrap()
                .unwrap()
                .primary_agent_snapshot,
            Some(persisted)
        );
    }
}
