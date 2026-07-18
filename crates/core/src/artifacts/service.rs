//! The artifact service: the one place callers create and read task
//! artifacts. Wraps [`crate::store::Store`] (metadata) and
//! [`ArtifactStorage`] (payload bytes) behind a single API that keeps the
//! two in sync — a payload is only ever written before its metadata row is
//! inserted, and a metadata-insert failure cleans up the orphaned payload it
//! would otherwise leave behind.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::artifacts::storage::{ArtifactError, ArtifactStorage};
use crate::artifacts::types::{
    ArtifactAccessRow, ArtifactCreator, ArtifactRecord, ArtifactReference, ArtifactStatus,
};
use crate::paths::{new_id, now_ms};
use crate::store::Store;

/// Byte caps enforced by [`ArtifactService`]. All three are plain byte
/// counts sourced from settings (`artifact_max_bytes`,
/// `artifact_session_max_bytes`, `artifact_read_max_bytes`); the service
/// itself has no opinion on defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArtifactConfig {
    /// Max size of a single artifact payload.
    pub max_bytes: u64,
    /// Max aggregate bytes of non-deleted artifacts a single source session
    /// may hold.
    pub session_max_bytes: u64,
    /// Max bytes returned by a single [`ArtifactService::read_range`] call,
    /// regardless of the requested `length`.
    pub read_max_bytes: u64,
}

/// Input to [`ArtifactService::create_artifact`].
#[derive(Debug, Clone)]
pub struct CreateArtifact {
    pub session_pk: String,
    pub source_message_seq: Option<i64>,
    pub source_run_id: Option<String>,
    pub creator: ArtifactCreator,
    pub creator_id: Option<String>,
    pub name: String,
    pub description: Option<String>,
    pub content_type: Option<String>,
    pub bytes: Vec<u8>,
}

/// The result of [`ArtifactService::read_range`]: the artifact's metadata
/// plus a capped slice of its payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ArtifactRead {
    pub artifact: ArtifactRecord,
    pub bytes: Vec<u8>,
    pub offset: u64,
    /// Total size of the stored payload, independent of `offset`/the cap.
    pub total_bytes: u64,
    /// `true` when the configured read cap held back bytes that were both
    /// requested and available.
    pub truncated: bool,
}

/// Strips any path components from `name`, keeping only the final
/// component, and rejects the result if it is empty or is exactly `.`/`..`.
/// Never treats the input as a real filesystem path — no lookups, no
/// canonicalization — this only guards against a caller-supplied display
/// name smuggling directory structure into `ArtifactRecord::name`.
fn safe_display_name(name: &str) -> Result<String, ArtifactError> {
    let base = name.rsplit(['/', '\\']).next().unwrap_or("");
    if base.is_empty() || base == "." || base == ".." {
        return Err(ArtifactError::InvalidName);
    }
    Ok(base.to_string())
}

/// Creates and reads task artifacts, keeping the [`Store`] metadata row and
/// the [`ArtifactStorage`] payload file in sync.
#[derive(Clone)]
pub struct ArtifactService {
    store: Arc<Store>,
    storage: ArtifactStorage,
    config: ArtifactConfig,
    quota_locks: Arc<Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>>,
}

/// Materialize saved user attachments as durable user-created artifacts.
///
/// The source session must exist and be live. Files are read only after that
/// validation and any failure is returned through the path-free artifact error
/// surface so callers can safely show it to users.
pub async fn ingest_saved_attachments(
    store: &Store,
    artifacts: &ArtifactService,
    session_pk: &str,
    message_seq: i64,
    saved: &[crate::attachments::SavedAttachment],
) -> Result<(), ArtifactError> {
    let session = store
        .get_session(session_pk)
        .await
        .map_err(|_| ArtifactError::StorageFailure)?
        .ok_or(ArtifactError::StorageFailure)?;
    if session.archived_at.is_some() {
        return Err(ArtifactError::ArchivedSource);
    }

    for attachment in saved {
        let bytes = tokio::fs::read(&attachment.path)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?;
        artifacts
            .create_artifact(CreateArtifact {
                session_pk: session_pk.to_string(),
                source_message_seq: Some(message_seq),
                source_run_id: None,
                creator: ArtifactCreator::User,
                creator_id: None,
                name: attachment.name.clone(),
                description: None,
                content_type: attachment.content_type.clone(),
                bytes,
            })
            .await?;
    }
    Ok(())
}

impl ArtifactService {
    pub fn new(store: Arc<Store>, storage: ArtifactStorage, config: ArtifactConfig) -> Self {
        Self {
            store,
            storage,
            config,
            quota_locks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    #[cfg(test)]
    fn fail_next_insert_for_test(&self) {
        self.store.fail_next_insert_artifact_for_test();
    }

    /// Persists a new artifact: validates the display name and size caps,
    /// writes the payload atomically under a fresh id-derived storage key,
    /// then inserts the metadata row. If the metadata insert fails, the
    /// just-written payload is deleted so no orphaned file survives a failed
    /// create; if that cleanup delete itself fails, the error reports the
    /// cleanup failure (see [`ArtifactError::MetadataInsertCleanupFailed`])
    /// rather than silently discarding it, without leaking the storage path.
    ///
    /// The per-session quota check-then-insert is serialized by an
    /// in-process async lock keyed on `source_session_pk`
    /// (`quota_locks`), one lock per session, created lazily. This makes
    /// concurrent creates for the same session unable to both observe the
    /// same "room left" snapshot and both proceed, without holding a SQLite
    /// transaction open across the async payload write (writes only need to
    /// be ordered relative to each other, not truly serialized against every
    /// unrelated session, so a global DB-level transaction spanning the disk
    /// I/O would cost concurrency for no additional safety). The lock is
    /// held from the quota check through the metadata insert, so a second
    /// call for the same session only sees the first call's fully committed
    /// (or fully failed-and-rolled-back) effect on `sum_active_artifact_bytes`.
    pub async fn create_artifact(
        &self,
        input: CreateArtifact,
    ) -> Result<ArtifactRecord, ArtifactError> {
        let name = safe_display_name(&input.name)?;

        let size_bytes = input.bytes.len() as u64;
        if size_bytes > self.config.max_bytes {
            return Err(ArtifactError::FileTooLarge {
                max_bytes: self.config.max_bytes,
            });
        }

        // Existing Task 2 callers may create test or imported artifacts before
        // their source session is persisted. Only a known archived session is
        // rejected; a missing session is left for the control-plane ingestion
        // path to validate when it has an active-session requirement.
        match self
            .store
            .get_session(&input.session_pk)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?
        {
            Some(session) if session.archived_at.is_some() => {
                return Err(ArtifactError::ArchivedSource);
            }
            Some(_) | None => {}
        }

        let quota_lock = {
            let mut locks = self.quota_locks.lock().expect("quota lock map poisoned");
            locks
                .entry(input.session_pk.clone())
                .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
                .clone()
        };
        let _quota_guard = quota_lock.lock().await;

        let existing = self
            .store
            .sum_active_artifact_bytes(&input.session_pk)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?;
        if existing.saturating_add(size_bytes) > self.config.session_max_bytes {
            return Err(ArtifactError::SessionQuotaExceeded {
                max_bytes: self.config.session_max_bytes,
            });
        }

        let id = new_id();
        let (storage_key, sha256) = self.storage.write_atomic(&id, &input.bytes).await?;

        let record = ArtifactRecord {
            id,
            source_session_pk: input.session_pk,
            source_message_seq: input.source_message_seq,
            source_run_id: input.source_run_id,
            creator: input.creator,
            creator_id: input.creator_id,
            name,
            description: input.description,
            content_type: input.content_type,
            size_bytes,
            sha256,
            storage_key: storage_key.clone(),
            status: ArtifactStatus::Available,
            created_at: now_ms(),
            deleted_at: None,
        };

        if let Err(e) = self.store.insert_artifact(&record).await {
            let cleanup = self.storage.delete(&storage_key).await;
            tracing::warn!("artifacts: metadata insert failed: {e}");
            return Err(if cleanup.is_err() {
                tracing::error!("artifacts: payload cleanup failed after metadata insert failure");
                ArtifactError::MetadataInsertCleanupFailed
            } else {
                ArtifactError::StorageFailure
            });
        }

        Ok(record)
    }

    /// Reads a complete artifact for an authenticated human download. Artifact
    /// writes already enforce the per-file cap, so this deliberately bypasses
    /// the smaller agent tool-output cap used by [`Self::read_range`].
    pub async fn read_full(&self, id: &str) -> Result<ArtifactRead, ArtifactError> {
        let artifact = self
            .store
            .artifact_by_id(id)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?
            .ok_or(ArtifactError::NotFound)?;
        let range = self
            .storage
            .read_range(
                &artifact.storage_key,
                0,
                artifact.size_bytes,
                artifact.size_bytes,
            )
            .await?;
        Ok(ArtifactRead {
            artifact,
            bytes: range.bytes,
            offset: 0,
            total_bytes: range.total_bytes,
            truncated: range.truncated,
        })
    }

    /// Reads up to `length` bytes (or up to the configured read cap when
    /// `length` is `None`) starting at `offset` from `id`'s stored payload.
    /// Looks the artifact up by id only — no archive/access-scope check is
    /// performed here; that belongs to a later task's caller.
    pub async fn read_range(
        &self,
        id: &str,
        offset: u64,
        length: Option<u64>,
    ) -> Result<ArtifactRead, ArtifactError> {
        let artifact = self
            .store
            .artifact_by_id(id)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?
            .ok_or(ArtifactError::NotFound)?;

        // An unbounded service read needs one sentinel byte beyond the cap so
        // storage can distinguish "exactly cap bytes remain" from "more
        // bytes remain" while still returning no more than the configured cap.
        let requested = length.unwrap_or_else(|| self.config.read_max_bytes.saturating_add(1));
        let range = self
            .storage
            .read_range(
                &artifact.storage_key,
                offset,
                requested,
                self.config.read_max_bytes,
            )
            .await?;

        Ok(ArtifactRead {
            artifact,
            bytes: range.bytes,
            offset,
            total_bytes: range.total_bytes,
            truncated: range.truncated,
        })
    }

    pub async fn share(
        &self,
        caller_session_pk: &str,
        artifact_or_reference_id: &str,
        target_session_pk: &str,
        actor: Option<&str>,
    ) -> Result<ArtifactReference, ArtifactError> {
        self.active_session(caller_session_pk).await?;
        self.active_session(target_session_pk).await?;
        let access = self
            .store
            .reference_for_session(artifact_or_reference_id, caller_session_pk)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?
            .ok_or(ArtifactError::AccessDenied)?;
        match access.artifact.status {
            ArtifactStatus::Available => {}
            ArtifactStatus::SourceArchived => return Err(ArtifactError::ArchivedSource),
            ArtifactStatus::Deleted => return Err(ArtifactError::Deleted),
        }
        self.active_session(&access.artifact.source_session_pk)
            .await?;

        if let Some(existing) = self
            .store
            .find_artifact_reference(&access.artifact.id, target_session_pk)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?
        {
            return Ok(existing);
        }

        let reference = ArtifactReference {
            id: new_id(),
            artifact_id: access.artifact.id,
            target_session_pk: target_session_pk.to_string(),
            shared_from_session_pk: caller_session_pk.to_string(),
            shared_by: actor.map(str::to_string),
            parent_reference_id: access.reference.map(|reference| reference.id),
            created_at: now_ms(),
        };
        if self
            .store
            .insert_artifact_reference(&reference)
            .await
            .is_ok()
        {
            return Ok(reference);
        }
        self.store
            .find_artifact_reference(&reference.artifact_id, target_session_pk)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?
            .ok_or(ArtifactError::StorageFailure)
    }

    pub async fn resolve_agent_access(
        &self,
        session_pk: &str,
        artifact_or_reference_id: &str,
    ) -> Result<ArtifactAccessRow, ArtifactError> {
        self.active_session(session_pk).await?;
        let access = self
            .store
            .reference_for_session(artifact_or_reference_id, session_pk)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?
            .ok_or(ArtifactError::AccessDenied)?;
        match access.artifact.status {
            ArtifactStatus::Available => {}
            ArtifactStatus::SourceArchived => return Err(ArtifactError::ArchivedSource),
            ArtifactStatus::Deleted => return Err(ArtifactError::SourceDeleted),
        }
        self.active_session(&access.artifact.source_session_pk)
            .await
            .map_err(|error| match error {
                ArtifactError::InactiveSession => ArtifactError::ArchivedSource,
                other => other,
            })?;
        Ok(access)
    }

    pub async fn purge_expired_archives(
        &self,
        now_ms: i64,
        retention_days: i64,
    ) -> Result<usize, ArtifactError> {
        let sessions = self
            .store
            .archived_sessions_past_retention(now_ms, retention_days)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?;
        let mut purged = 0;
        for session_pk in sessions {
            let artifacts = self
                .store
                .list_source_artifacts(&session_pk)
                .await
                .map_err(|_| ArtifactError::StorageFailure)?;
            for artifact in artifacts
                .iter()
                .filter(|artifact| artifact.status != ArtifactStatus::Deleted)
            {
                self.storage.delete(&artifact.storage_key).await?;
            }
            self.store
                .mark_source_artifacts_deleted(&session_pk, now_ms)
                .await
                .map_err(|_| ArtifactError::StorageFailure)?;
            if self
                .store
                .delete_session_after_artifact_purge(&session_pk)
                .await
                .map_err(|_| ArtifactError::StorageFailure)?
            {
                purged += 1;
            }
        }
        Ok(purged)
    }

    async fn active_session(&self, session_pk: &str) -> Result<(), ArtifactError> {
        let session = self
            .store
            .get_session(session_pk)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?
            .ok_or(ArtifactError::InactiveSession)?;
        if session.archived_at.is_some() {
            return Err(ArtifactError::InactiveSession);
        }
        Ok(())
    }

    /// Reads `id`'s payload on an agent's behalf: unlike [`Self::read_range`],
    /// this enforces the access rules an agent must never bypass — a deleted
    /// artifact is never readable, and an artifact whose source session is
    /// currently archived is denied until the session is restored (see
    /// [`crate::control::ControlPlane::archive_session`] /
    /// [`crate::control::ControlPlane::restore_session`]) — then delegates
    /// the actual byte-range fetch to [`Self::read_range`].
    pub async fn read_for_agent(
        &self,
        session_pk: &str,
        id: &str,
        offset: u64,
        length: Option<u64>,
    ) -> Result<ArtifactRead, ArtifactError> {
        let access =
            self.resolve_agent_access(session_pk, id)
                .await
                .map_err(|error| match error {
                    ArtifactError::InactiveSession => ArtifactError::ArchivedSource,
                    ArtifactError::SourceDeleted => ArtifactError::Deleted,
                    other => other,
                })?;
        let artifact = access.artifact;
        if artifact.status == ArtifactStatus::Deleted {
            return Err(ArtifactError::Deleted);
        }

        self.read_range(&artifact.id, offset, length).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::types::ArtifactCreator;
    use sha2::Digest;

    async fn service(config: ArtifactConfig) -> (tempfile::TempDir, Arc<Store>, ArtifactService) {
        let storage_dir = tempfile::tempdir().unwrap();
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db_file.path()).await.unwrap());
        let storage = ArtifactStorage::new(storage_dir.path());
        (
            storage_dir,
            Arc::clone(&store),
            ArtifactService::new(store, storage, config),
        )
    }

    fn sample_session() -> crate::domain::Session {
        crate::domain::Session {
            session_pk: "sess-1".into(),
            primary_agent_id: None,
            primary_agent_snapshot: None,
            project_id: None,
            agent_session_id: None,
            worktree_path: None,
            branch: None,
            title: None,
            status: crate::domain::SessionStatus::Idle,
            perm_mode: crate::domain::PermMode::Default,
            started_by: None,
            created_at: Some(1),
            last_active: Some(1),
            resume_attempts: 0,
            branch_owned: false,
            kind: crate::domain::SessionKind::Chat,
            speaker: None,
            agent: None,
            parent_session_pk: None,
            archived_at: None,
        }
    }

    fn base_input(bytes: &[u8]) -> CreateArtifact {
        CreateArtifact {
            session_pk: "sess-1".into(),
            source_message_seq: Some(7),
            source_run_id: Some("run-1".into()),
            creator: ArtifactCreator::Agent,
            creator_id: Some("ada".into()),
            name: "report.md".into(),
            description: Some("summary".into()),
            content_type: Some("text/markdown".into()),
            bytes: bytes.to_vec(),
        }
    }

    fn default_config() -> ArtifactConfig {
        ArtifactConfig {
            max_bytes: 1_000,
            session_max_bytes: 10_000,
            read_max_bytes: 1_000,
        }
    }

    #[tokio::test]
    async fn ingest_saved_attachments_creates_user_artifacts_with_message_provenance() {
        let (dir, store, svc) = service(default_config()).await;
        store.insert_session(sample_session()).await.unwrap();
        let attachment_path = dir.path().join("source-note.txt");
        tokio::fs::write(&attachment_path, b"attachment bytes")
            .await
            .unwrap();

        ingest_saved_attachments(
            &store,
            &svc,
            "sess-1",
            7,
            &[crate::attachments::SavedAttachment {
                path: attachment_path,
                name: "original-note.txt".into(),
                content_type: Some("text/plain".into()),
                size: 16,
            }],
        )
        .await
        .unwrap();

        let artifacts = store.artifacts_for_session("sess-1").await.unwrap();
        assert_eq!(artifacts.len(), 1);
        let record = &artifacts[0].artifact;
        assert_eq!(record.name, "original-note.txt");
        assert_eq!(record.source_session_pk, "sess-1");
        assert_eq!(record.source_message_seq, Some(7));
        assert_eq!(record.creator, ArtifactCreator::User);
        assert_eq!(
            svc.read_range(&record.id, 0, None).await.unwrap().bytes,
            b"attachment bytes"
        );
    }

    #[tokio::test]
    async fn ingest_saved_attachments_rejects_archived_sessions_without_an_artifact() {
        let (dir, store, svc) = service(default_config()).await;
        store.insert_session(sample_session()).await.unwrap();
        store.archive_session("sess-1", 2).await.unwrap();
        let attachment_path = dir.path().join("archived.txt");
        tokio::fs::write(&attachment_path, b"archived")
            .await
            .unwrap();

        let error = ingest_saved_attachments(
            &store,
            &svc,
            "sess-1",
            7,
            &[crate::attachments::SavedAttachment {
                path: attachment_path,
                name: "archived.txt".into(),
                content_type: Some("text/plain".into()),
                size: 8,
            }],
        )
        .await
        .unwrap_err();

        assert!(error.to_string().contains("archived"));
        assert!(store
            .artifacts_for_session("sess-1")
            .await
            .unwrap()
            .is_empty());
    }

    #[tokio::test]
    async fn create_artifact_strips_path_components_from_the_display_name() {
        let (_dir, _store, svc) = service(default_config()).await;
        let mut input = base_input(b"hello");
        input.name = "../../etc/passwd".into();

        let record = svc.create_artifact(input).await.unwrap();
        assert_eq!(record.name, "passwd");
    }

    #[tokio::test]
    async fn create_artifact_rejects_empty_or_dot_names() {
        let (_dir, _store, svc) = service(default_config()).await;
        for bad in ["", ".", "..", "a/../..", "a/./"] {
            let mut input = base_input(b"hello");
            input.name = bad.into();
            let err = svc.create_artifact(input).await.unwrap_err();
            assert_eq!(err, ArtifactError::InvalidName, "name={bad:?}");
        }
    }

    #[tokio::test]
    async fn create_artifact_writes_payload_under_the_generated_id_with_sha256() {
        let (dir, _store, svc) = service(default_config()).await;
        let record = svc.create_artifact(base_input(b"hello")).await.unwrap();

        assert_eq!(record.storage_key, record.id);
        assert_eq!(
            record.sha256,
            format!("{:x}", sha2::Sha256::digest(b"hello"))
        );
        let on_disk = std::fs::read(dir.path().join(&record.storage_key)).unwrap();
        assert_eq!(on_disk, b"hello");

        let fetched = svc.read_range(&record.id, 0, None).await.unwrap();
        assert_eq!(fetched.artifact, record);
    }

    #[tokio::test]
    async fn create_artifact_enforces_the_per_file_cap() {
        let (_dir, _store, svc) = service(ArtifactConfig {
            max_bytes: 4,
            session_max_bytes: 10_000,
            read_max_bytes: 1_000,
        })
        .await;

        let err = svc.create_artifact(base_input(b"hello")).await.unwrap_err();
        assert_eq!(err, ArtifactError::FileTooLarge { max_bytes: 4 });
    }

    #[tokio::test]
    async fn concurrent_creates_cannot_exceed_session_aggregate_quota() {
        let (_dir, _store, svc) = service(ArtifactConfig {
            max_bytes: 1_000,
            session_max_bytes: 5,
            read_max_bytes: 1_000,
        })
        .await;
        let first = svc.create_artifact(base_input(b"hello"));
        let second = svc.create_artifact(base_input(b"world"));
        let (left, right) = tokio::join!(first, second);
        assert_eq!(left.is_ok() as u8 + right.is_ok() as u8, 1);
        let quota_errors = [left, right]
            .into_iter()
            .filter(|result| *result == Err(ArtifactError::SessionQuotaExceeded { max_bytes: 5 }))
            .count();
        assert_eq!(quota_errors, 1);
    }

    #[tokio::test]
    async fn metadata_failure_reports_cleanup_failure_without_path_leak() {
        let (dir, _store, svc) = service(default_config()).await;
        svc.fail_next_insert_for_test();
        svc.storage.fail_next_delete_for_test();

        let err = svc.create_artifact(base_input(b"hello")).await.unwrap_err();
        assert_eq!(err, ArtifactError::MetadataInsertCleanupFailed);
        assert!(err.to_string().contains("cleanup"));
        assert!(!err
            .to_string()
            .contains(dir.path().to_string_lossy().as_ref()));
    }

    #[tokio::test]
    async fn create_artifact_enforces_the_session_aggregate_quota() {
        let (_dir, _store, svc) = service(ArtifactConfig {
            max_bytes: 1_000,
            session_max_bytes: 8,
            read_max_bytes: 1_000,
        })
        .await;

        svc.create_artifact(base_input(b"hello")).await.unwrap();

        let err = svc
            .create_artifact(base_input(b"world!"))
            .await
            .unwrap_err();
        assert_eq!(err, ArtifactError::SessionQuotaExceeded { max_bytes: 8 });
    }

    #[tokio::test]
    async fn create_artifact_allows_a_second_artifact_within_the_remaining_quota() {
        let (_dir, _store, svc) = service(ArtifactConfig {
            max_bytes: 1_000,
            session_max_bytes: 10,
            read_max_bytes: 1_000,
        })
        .await;

        svc.create_artifact(base_input(b"hello")).await.unwrap();
        // 5 + 5 = 10, exactly at (not over) the quota.
        let second = svc.create_artifact(base_input(b"world")).await.unwrap();
        assert_eq!(second.size_bytes, 5);
    }

    #[tokio::test]
    async fn create_artifact_cleans_up_the_payload_when_metadata_insert_fails() {
        let (dir, _store, svc) = service(default_config()).await;
        svc.fail_next_insert_for_test();

        let err = svc.create_artifact(base_input(b"hello")).await.unwrap_err();
        assert_eq!(err, ArtifactError::StorageFailure);
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(entries.is_empty(), "payload should be cleaned up");
    }

    #[tokio::test]
    async fn read_full_bypasses_the_agent_output_cap_for_human_downloads() {
        let (_dir, _store, svc) = service(ArtifactConfig {
            max_bytes: 1_000,
            session_max_bytes: 10_000,
            read_max_bytes: 3,
        })
        .await;
        let record = svc
            .create_artifact(base_input(b"0123456789"))
            .await
            .unwrap();

        let read = svc.read_full(&record.id).await.unwrap();
        assert_eq!(read.bytes, b"0123456789");
        assert!(!read.truncated);
    }

    #[tokio::test]
    async fn read_range_clamps_to_the_configured_cap() {
        let (_dir, _store, svc) = service(ArtifactConfig {
            max_bytes: 1_000,
            session_max_bytes: 10_000,
            read_max_bytes: 3,
        })
        .await;
        let record = svc
            .create_artifact(base_input(b"0123456789"))
            .await
            .unwrap();

        let read = svc.read_range(&record.id, 2, None).await.unwrap();
        assert_eq!(read.bytes, b"234");
        assert_eq!(read.offset, 2);
        assert_eq!(read.total_bytes, 10);
        assert!(read.truncated);
    }

    #[tokio::test]
    async fn read_range_honors_a_smaller_requested_length_than_the_cap() {
        let (_dir, _store, svc) = service(default_config()).await;
        let record = svc
            .create_artifact(base_input(b"0123456789"))
            .await
            .unwrap();

        let read = svc.read_range(&record.id, 0, Some(4)).await.unwrap();
        assert_eq!(read.bytes, b"0123");
        assert!(!read.truncated);
    }

    #[tokio::test]
    async fn read_range_rejects_unknown_ids() {
        let (_dir, _store, svc) = service(default_config()).await;
        let err = svc.read_range("does-not-exist", 0, None).await.unwrap_err();
        assert_eq!(err, ArtifactError::NotFound);
    }

    #[tokio::test]
    async fn create_artifact_rejects_a_known_archived_source_session() {
        let (_dir, store, svc) = service(default_config()).await;
        store.insert_session(sample_session()).await.unwrap();
        assert!(store.archive_session("sess-1", 10).await.unwrap());

        assert_eq!(
            svc.create_artifact(base_input(b"hello")).await.unwrap_err(),
            ArtifactError::ArchivedSource
        );
    }

    fn session_with_id(id: &str) -> crate::domain::Session {
        let mut session = sample_session();
        session.session_pk = id.to_string();
        session
    }

    #[tokio::test]
    async fn share_is_idempotent_and_preserves_reference_chain() {
        let (_dir, store, svc) = service(default_config()).await;
        for id in ["sess-1", "sess-2", "sess-3"] {
            store.insert_session(session_with_id(id)).await.unwrap();
        }
        let artifact = svc.create_artifact(base_input(b"hello")).await.unwrap();
        let first = svc
            .share("sess-1", &artifact.id, "sess-2", Some("ada"))
            .await
            .unwrap();
        let duplicate = svc
            .share("sess-1", &artifact.id, "sess-2", Some("ada"))
            .await
            .unwrap();
        assert_eq!(first, duplicate);
        let second = svc
            .share("sess-2", &first.id, "sess-3", Some("bea"))
            .await
            .unwrap();
        assert_eq!(second.artifact_id, artifact.id);
        assert_eq!(second.shared_from_session_pk, "sess-2");
        assert_eq!(second.parent_reference_id, Some(first.id));
        assert!(svc.resolve_agent_access("sess-3", &second.id).await.is_ok());
    }

    #[tokio::test]
    async fn archive_denies_references_and_retention_deletes_source_payload() {
        let (dir, store, svc) = service(default_config()).await;
        for id in ["sess-1", "sess-2"] {
            store.insert_session(session_with_id(id)).await.unwrap();
        }
        let artifact = svc.create_artifact(base_input(b"hello")).await.unwrap();
        let reference = svc
            .share("sess-1", &artifact.id, "sess-2", None)
            .await
            .unwrap();
        assert!(store.archive_session("sess-1", 1_000).await.unwrap());
        assert_eq!(
            svc.resolve_agent_access("sess-2", &reference.id)
                .await
                .unwrap_err(),
            ArtifactError::ArchivedSource
        );
        assert!(store.restore_session("sess-1").await.unwrap());
        assert!(svc
            .resolve_agent_access("sess-2", &reference.id)
            .await
            .is_ok());
        assert!(store.archive_session("sess-1", 1_000).await.unwrap());

        assert_eq!(svc.purge_expired_archives(1_000, 0).await.unwrap(), 1);
        assert!(!dir.path().join(&artifact.storage_key).exists());
        assert!(store.get_session("sess-1").await.unwrap().is_none());
        let listed = store.artifacts_for_session("sess-2").await.unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].artifact.status, ArtifactStatus::Deleted);
        assert_eq!(svc.purge_expired_archives(1_000, 0).await.unwrap(), 0);
    }

    #[tokio::test]
    async fn read_for_agent_denies_archived_sources_then_allows_restored_sources() {
        let (_dir, store, svc) = service(default_config()).await;
        store.insert_session(sample_session()).await.unwrap();
        let record = svc.create_artifact(base_input(b"hello")).await.unwrap();

        assert_eq!(
            svc.read_for_agent("sess-1", &record.id, 0, None)
                .await
                .unwrap()
                .bytes,
            b"hello"
        );
        assert!(store.archive_session("sess-1", 10).await.unwrap());
        assert_eq!(
            svc.read_for_agent("sess-1", &record.id, 0, None)
                .await
                .unwrap_err(),
            ArtifactError::ArchivedSource
        );
        assert!(store.restore_session("sess-1").await.unwrap());
        assert_eq!(
            svc.read_for_agent("sess-1", &record.id, 0, None)
                .await
                .unwrap()
                .bytes,
            b"hello"
        );
    }

    #[tokio::test]
    async fn read_for_agent_denies_deleted_artifacts() {
        let (_dir, store, svc) = service(default_config()).await;
        store.insert_session(sample_session()).await.unwrap();
        let record = svc.create_artifact(base_input(b"hello")).await.unwrap();
        assert_eq!(
            store
                .set_source_artifact_status(
                    "sess-1",
                    ArtifactStatus::Available,
                    ArtifactStatus::Deleted,
                )
                .await
                .unwrap(),
            1
        );

        assert_eq!(
            svc.read_for_agent("sess-1", &record.id, 0, None)
                .await
                .unwrap_err(),
            ArtifactError::Deleted
        );
    }

    #[test]
    fn artifact_error_display_never_leaks_paths() {
        // Every variant's Display is a fixed, path-free message — assert
        // none of them could possibly embed a filesystem path by
        // construction (no Display impl here formats a `Path`/`PathBuf`).
        let all = [
            ArtifactError::InvalidName,
            ArtifactError::InvalidStorageKey,
            ArtifactError::FileTooLarge { max_bytes: 1 },
            ArtifactError::SessionQuotaExceeded { max_bytes: 1 },
            ArtifactError::NotFound,
            ArtifactError::ArchivedSource,
            ArtifactError::Deleted,
            ArtifactError::MetadataInsertCleanupFailed,
            ArtifactError::StorageFailure,
        ];
        for err in all {
            let text = err.to_string();
            assert!(!text.contains(std::path::MAIN_SEPARATOR));
            assert!(!text.contains('/'));
        }
    }
}
