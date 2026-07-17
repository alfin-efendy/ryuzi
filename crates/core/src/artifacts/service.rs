//! The artifact service: the one place callers create and read task
//! artifacts. Wraps [`crate::store::Store`] (metadata) and
//! [`ArtifactStorage`] (payload bytes) behind a single API that keeps the
//! two in sync — a payload is only ever written before its metadata row is
//! inserted, and a metadata-insert failure cleans up the orphaned payload it
//! would otherwise leave behind.

use std::sync::Arc;

use crate::artifacts::storage::{ArtifactError, ArtifactStorage};
use crate::artifacts::types::{ArtifactCreator, ArtifactRecord, ArtifactStatus};
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
}

impl ArtifactService {
    pub fn new(store: Arc<Store>, storage: ArtifactStorage, config: ArtifactConfig) -> Self {
        Self {
            store,
            storage,
            config,
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
    /// create.
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
            let _ = self.storage.delete(&storage_key).await;
            tracing::warn!("artifacts: metadata insert failed, payload cleaned up: {e}");
            return Err(ArtifactError::StorageFailure);
        }

        Ok(record)
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
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::artifacts::types::ArtifactCreator;
    use sha2::Digest;

    async fn service(config: ArtifactConfig) -> (tempfile::TempDir, ArtifactService) {
        let storage_dir = tempfile::tempdir().unwrap();
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(db_file.path()).await.unwrap());
        let storage = ArtifactStorage::new(storage_dir.path());
        (storage_dir, ArtifactService::new(store, storage, config))
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
    async fn create_artifact_strips_path_components_from_the_display_name() {
        let (_dir, svc) = service(default_config()).await;
        let mut input = base_input(b"hello");
        input.name = "../../etc/passwd".into();

        let record = svc.create_artifact(input).await.unwrap();
        assert_eq!(record.name, "passwd");
    }

    #[tokio::test]
    async fn create_artifact_rejects_empty_or_dot_names() {
        let (_dir, svc) = service(default_config()).await;
        for bad in ["", ".", "..", "a/../..", "a/./"] {
            let mut input = base_input(b"hello");
            input.name = bad.into();
            let err = svc.create_artifact(input).await.unwrap_err();
            assert_eq!(err, ArtifactError::InvalidName, "name={bad:?}");
        }
    }

    #[tokio::test]
    async fn create_artifact_writes_payload_under_the_generated_id_with_sha256() {
        let (dir, svc) = service(default_config()).await;
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
        let (_dir, svc) = service(ArtifactConfig {
            max_bytes: 4,
            session_max_bytes: 10_000,
            read_max_bytes: 1_000,
        })
        .await;

        let err = svc.create_artifact(base_input(b"hello")).await.unwrap_err();
        assert_eq!(err, ArtifactError::FileTooLarge { max_bytes: 4 });
    }

    #[tokio::test]
    async fn create_artifact_enforces_the_session_aggregate_quota() {
        let (_dir, svc) = service(ArtifactConfig {
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
        let (_dir, svc) = service(ArtifactConfig {
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
        let (dir, svc) = service(default_config()).await;
        svc.fail_next_insert_for_test();

        let err = svc.create_artifact(base_input(b"hello")).await.unwrap_err();
        assert_eq!(err, ArtifactError::StorageFailure);
        let entries: Vec<_> = std::fs::read_dir(dir.path()).unwrap().collect();
        assert!(entries.is_empty(), "payload should be cleaned up");
    }

    #[tokio::test]
    async fn read_range_clamps_to_the_configured_cap() {
        let (_dir, svc) = service(ArtifactConfig {
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
        let (_dir, svc) = service(default_config()).await;
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
        let (_dir, svc) = service(default_config()).await;
        let err = svc.read_range("does-not-exist", 0, None).await.unwrap_err();
        assert_eq!(err, ArtifactError::NotFound);
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
            ArtifactError::StorageFailure,
        ];
        for err in all {
            let text = err.to_string();
            assert!(!text.contains(std::path::MAIN_SEPARATOR));
            assert!(!text.contains('/'));
        }
    }
}
