//! Persistence-layer types for the task artifact store.
//!
//! These types describe the DB-facing shape of artifacts and their
//! cross-session references. They intentionally carry no behavior beyond
//! strict string <-> enum conversion for SQLite columns.

/// Who produced an artifact: the human user or an agent run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactCreator {
    User,
    Agent,
}

impl ArtifactCreator {
    pub fn as_db(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Agent => "agent",
        }
    }

    pub fn from_db(value: &str) -> rusqlite::Result<Self> {
        match value {
            "user" => Ok(Self::User),
            "agent" => Ok(Self::Agent),
            _ => Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("invalid artifact creator `{value}`").into(),
            )),
        }
    }
}

/// Lifecycle state of a stored artifact.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactStatus {
    Available,
    SourceArchived,
    Deleted,
}

impl ArtifactStatus {
    pub fn as_db(self) -> &'static str {
        match self {
            Self::Available => "available",
            Self::SourceArchived => "source-archived",
            Self::Deleted => "deleted",
        }
    }

    pub fn from_db(value: &str) -> rusqlite::Result<Self> {
        match value {
            "available" => Ok(Self::Available),
            "source-archived" => Ok(Self::SourceArchived),
            "deleted" => Ok(Self::Deleted),
            _ => Err(rusqlite::Error::FromSqlConversionFailure(
                0,
                rusqlite::types::Type::Text,
                format!("invalid artifact status `{value}`").into(),
            )),
        }
    }
}

/// A single stored artifact produced within a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRecord {
    pub id: String,
    pub source_session_pk: String,
    pub source_message_seq: Option<i64>,
    pub source_run_id: Option<String>,
    pub creator: ArtifactCreator,
    pub creator_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub content_type: Option<String>,
    pub size_bytes: u64,
    pub sha256: String,
    pub storage_key: String,
    pub status: ArtifactStatus,
    pub created_at: i64,
    pub deleted_at: Option<i64>,
}

/// A reference sharing an artifact into another session's scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactReference {
    pub id: String,
    pub artifact_id: String,
    pub target_session_pk: String,
    pub shared_from_session_pk: String,
    pub shared_by: Option<String>,
    pub parent_reference_id: Option<String>,
    pub created_at: i64,
}

/// One row of a session's artifact listing: either an artifact the session
/// originated (`reference` is `None`) or one shared into the session via a
/// reference (`reference` is `Some`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactListRow {
    pub artifact: ArtifactRecord,
    pub reference: Option<ArtifactReference>,
}

/// The result of resolving an artifact (or reference) id against a caller's
/// session scope: the original artifact, plus the reference used to reach it
/// (`None` when the caller is the artifact's originating session).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactAccessRow {
    pub artifact: ArtifactRecord,
    pub reference: Option<ArtifactReference>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn artifact_creator_round_trips_known_values() {
        assert_eq!(
            ArtifactCreator::from_db("user").unwrap(),
            ArtifactCreator::User
        );
        assert_eq!(
            ArtifactCreator::from_db("agent").unwrap(),
            ArtifactCreator::Agent
        );
        assert_eq!(ArtifactCreator::User.as_db(), "user");
        assert_eq!(ArtifactCreator::Agent.as_db(), "agent");
    }

    #[test]
    fn artifact_status_round_trips_known_values() {
        assert_eq!(
            ArtifactStatus::from_db("available").unwrap(),
            ArtifactStatus::Available
        );
        assert_eq!(
            ArtifactStatus::from_db("source-archived").unwrap(),
            ArtifactStatus::SourceArchived
        );
        assert_eq!(
            ArtifactStatus::from_db("deleted").unwrap(),
            ArtifactStatus::Deleted
        );
        assert_eq!(ArtifactStatus::Available.as_db(), "available");
        assert_eq!(ArtifactStatus::SourceArchived.as_db(), "source-archived");
        assert_eq!(ArtifactStatus::Deleted.as_db(), "deleted");
    }

    #[test]
    fn artifact_status_rejects_unknown_value() {
        let err = ArtifactStatus::from_db("archived-forever").unwrap_err();
        match err {
            rusqlite::Error::FromSqlConversionFailure(_, ty, source) => {
                assert_eq!(ty, rusqlite::types::Type::Text);
                assert!(source.to_string().contains("archived-forever"));
            }
            other => panic!("expected FromSqlConversionFailure, got {other:?}"),
        }
    }
}
