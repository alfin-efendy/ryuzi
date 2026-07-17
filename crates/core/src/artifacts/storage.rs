//! Filesystem storage primitives for artifact payloads.
//!
//! Payloads live under one configured root, one file per artifact, named by
//! a safe, ID-only storage key (never a caller-supplied display name or
//! path). Writes are atomic: content is written to a temporary sibling
//! file, `fsync`'d, then renamed into place, so a concurrent reader never
//! observes a partially-written payload and a crash mid-write leaves only
//! an orphaned temp file behind, never a corrupt final file.

use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use tokio::io::{AsyncReadExt, AsyncSeekExt, AsyncWrite, AsyncWriteExt};

/// Structured, path-free failure surface for artifact storage and service
/// operations. `Display` never includes filesystem paths or raw OS error
/// text, so it is safe to surface directly to API/tool callers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArtifactError {
    /// The proposed display name is empty (or became empty) once path
    /// separators / traversal segments are stripped.
    InvalidName,
    /// A storage key is empty, contains a path separator or `..` segment,
    /// or otherwise isn't a bare, safe identifier.
    InvalidStorageKey,
    /// The payload exceeds the configured per-artifact byte cap
    /// (`artifact_max_bytes`).
    FileTooLarge { max_bytes: u64 },
    /// Persisting the payload would exceed the source session's aggregate
    /// artifact byte quota (`artifact_session_max_bytes`).
    SessionQuotaExceeded { max_bytes: u64 },
    /// No artifact (or, at the storage layer, no payload file) exists for
    /// the requested id / storage key.
    NotFound,
    /// The underlying filesystem operation failed. No path or OS error text
    /// is carried, to avoid leaking local paths to callers.
    StorageFailure,
}

impl std::fmt::Display for ArtifactError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidName => write!(f, "artifact name is invalid or empty"),
            Self::InvalidStorageKey => write!(f, "artifact storage key is invalid"),
            Self::FileTooLarge { max_bytes } => {
                write!(f, "artifact exceeds the maximum size of {max_bytes} bytes")
            }
            Self::SessionQuotaExceeded { max_bytes } => write!(
                f,
                "session artifact storage quota of {max_bytes} bytes would be exceeded"
            ),
            Self::NotFound => write!(f, "artifact not found"),
            Self::StorageFailure => write!(f, "artifact storage operation failed"),
        }
    }
}

impl std::error::Error for ArtifactError {}

/// The result of a capped, offset-based read of a stored payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadRange {
    pub bytes: Vec<u8>,
    /// Total size of the stored payload, independent of `offset`/the cap.
    pub total_bytes: u64,
    /// `true` when the configured read cap held back bytes that were both
    /// requested and available (i.e. more data existed at `offset..` than
    /// `bytes` contains, purely because of the cap).
    pub truncated: bool,
}

/// `true` iff `key` is safe to use as a bare filename directly under the
/// storage root: non-empty, ASCII alphanumeric/`-`/`_` only, no path
/// separators, and no `.` (which also rules out `.`/`..`).
fn is_safe_storage_key(key: &str) -> bool {
    !key.is_empty()
        && key
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Owns one filesystem root that artifact payloads are stored under, one
/// file per artifact, named by a safe storage key derived from (and, today,
/// equal to) the artifact id.
#[derive(Debug, Clone)]
pub struct ArtifactStorage {
    root: PathBuf,
}

impl ArtifactStorage {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    fn resolve(&self, storage_key: &str) -> Result<PathBuf, ArtifactError> {
        if !is_safe_storage_key(storage_key) {
            return Err(ArtifactError::InvalidStorageKey);
        }
        Ok(self.root.join(storage_key))
    }

    /// Atomically writes `bytes` under a storage key derived from `id`
    /// (today, the id itself — `id` must already be a safe, ID-only
    /// identifier), computing its SHA-256 digest along the way. Returns
    /// `(storage_key, sha256_hex)` on success.
    ///
    /// Writes go to a temporary sibling file in the same directory (so the
    /// final `rename` is same-filesystem and atomic), which is `fsync`'d
    /// before the rename and removed if any step — including the rename
    /// itself — fails.
    pub async fn write_atomic(
        &self,
        id: &str,
        bytes: &[u8],
    ) -> Result<(String, String), ArtifactError> {
        let storage_key = id.to_string();
        let final_path = self.resolve(&storage_key)?;

        tokio::fs::create_dir_all(&self.root)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?;

        let sha256 = format!("{:x}", Sha256::digest(bytes));

        let tmp_name = format!(".{storage_key}.tmp-{}", crate::paths::new_id());
        let tmp_path = self.root.join(&tmp_name);

        let write_result: Result<(), ArtifactError> = async {
            let mut file = tokio::fs::File::create(&tmp_path)
                .await
                .map_err(|_| ArtifactError::StorageFailure)?;
            file.write_all(bytes)
                .await
                .map_err(|_| ArtifactError::StorageFailure)?;
            file.sync_all()
                .await
                .map_err(|_| ArtifactError::StorageFailure)?;
            drop(file);
            tokio::fs::rename(&tmp_path, &final_path)
                .await
                .map_err(|_| ArtifactError::StorageFailure)
        }
        .await;

        if write_result.is_err() {
            let _ = tokio::fs::remove_file(&tmp_path).await;
        }
        write_result?;

        Ok((storage_key, sha256))
    }

    /// Reads up to `length` bytes starting at `offset` from the payload
    /// stored under `storage_key`, clamped to both the remaining bytes in
    /// the file and `cap` — whichever is smaller. `truncated` reports
    /// whether the cap (rather than end-of-file) is what limited the read.
    pub async fn read_range(
        &self,
        storage_key: &str,
        offset: u64,
        length: u64,
        cap: u64,
    ) -> Result<ReadRange, ArtifactError> {
        let path = self.resolve(storage_key)?;
        let mut file = tokio::fs::File::open(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ArtifactError::NotFound
            } else {
                ArtifactError::StorageFailure
            }
        })?;
        let total_bytes = file
            .metadata()
            .await
            .map_err(|_| ArtifactError::StorageFailure)?
            .len();

        if offset >= total_bytes {
            return Ok(ReadRange {
                bytes: Vec::new(),
                total_bytes,
                truncated: false,
            });
        }

        let remaining = total_bytes - offset;
        let requested = length.min(remaining);
        let capped = requested.min(cap);
        let truncated = capped < requested;

        file.seek(std::io::SeekFrom::Start(offset))
            .await
            .map_err(|_| ArtifactError::StorageFailure)?;
        let mut bytes = vec![0u8; capped as usize];
        file.read_exact(&mut bytes)
            .await
            .map_err(|_| ArtifactError::StorageFailure)?;

        Ok(ReadRange {
            bytes,
            total_bytes,
            truncated,
        })
    }

    /// Removes the payload stored under `storage_key`. A missing file is
    /// not an error (delete is idempotent).
    pub async fn delete(&self, storage_key: &str) -> Result<(), ArtifactError> {
        let path = self.resolve(storage_key)?;
        match tokio::fs::remove_file(&path).await {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(ArtifactError::StorageFailure),
        }
    }

    /// Streams the full payload stored under `storage_key` to `writer`,
    /// returning the number of bytes copied. Intended for future
    /// export/download paths that need to avoid buffering a whole payload
    /// in memory.
    pub async fn stream_to<W>(
        &self,
        storage_key: &str,
        writer: &mut W,
    ) -> Result<u64, ArtifactError>
    where
        W: AsyncWrite + Unpin,
    {
        let path = self.resolve(storage_key)?;
        let mut file = tokio::fs::File::open(&path).await.map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                ArtifactError::NotFound
            } else {
                ArtifactError::StorageFailure
            }
        })?;
        tokio::io::copy(&mut file, writer)
            .await
            .map_err(|_| ArtifactError::StorageFailure)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn temp_storage() -> (tempfile::TempDir, ArtifactStorage) {
        let dir = tempfile::tempdir().unwrap();
        let storage = ArtifactStorage::new(dir.path());
        (dir, storage)
    }

    #[tokio::test]
    async fn write_atomic_persists_bytes_and_computes_sha256() {
        let (_dir, storage) = temp_storage();
        let bytes = b"hello artifact world";
        let (storage_key, sha256) = storage.write_atomic("artifact-id-1", bytes).await.unwrap();
        assert_eq!(storage_key, "artifact-id-1");
        assert_eq!(sha256, format!("{:x}", Sha256::digest(bytes)));

        let on_disk = tokio::fs::read(storage.root().join(&storage_key))
            .await
            .unwrap();
        assert_eq!(on_disk, bytes);

        // No leftover temp file next to the final one.
        let mut entries = tokio::fs::read_dir(storage.root()).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        assert_eq!(names, vec![storage_key]);
    }

    #[tokio::test]
    async fn write_atomic_rejects_traversal_and_invalid_ids() {
        let (_dir, storage) = temp_storage();
        for bad in ["../escape", "a/b", "a\\b", "", "..", ".", "with space"] {
            let err = storage.write_atomic(bad, b"x").await.unwrap_err();
            assert_eq!(err, ArtifactError::InvalidStorageKey, "id={bad:?}");
        }
    }

    #[tokio::test]
    async fn write_atomic_cleans_up_temp_file_on_rename_failure() {
        let (_dir, storage) = temp_storage();
        // A directory occupying the destination filename makes the final
        // `rename` fail on every platform (rename onto a directory is
        // rejected), letting us exercise the cleanup path deterministically.
        tokio::fs::create_dir_all(storage.root().join("blocked-id"))
            .await
            .unwrap();

        let err = storage
            .write_atomic("blocked-id", b"payload")
            .await
            .unwrap_err();
        assert_eq!(err, ArtifactError::StorageFailure);

        let mut entries = tokio::fs::read_dir(storage.root()).await.unwrap();
        let mut names = Vec::new();
        while let Some(entry) = entries.next_entry().await.unwrap() {
            names.push(entry.file_name().to_string_lossy().to_string());
        }
        // Only the pre-existing blocking directory remains — no `.tmp-*`
        // sibling was left behind.
        assert_eq!(names, vec!["blocked-id"]);
    }

    #[tokio::test]
    async fn read_range_returns_requested_slice() {
        let (_dir, storage) = temp_storage();
        storage.write_atomic("doc-1", b"0123456789").await.unwrap();

        let result = storage.read_range("doc-1", 2, 4, 1_000).await.unwrap();
        assert_eq!(result.bytes, b"2345");
        assert_eq!(result.total_bytes, 10);
        assert!(!result.truncated);
    }

    #[tokio::test]
    async fn read_range_is_capped_and_reports_truncation() {
        let (_dir, storage) = temp_storage();
        storage.write_atomic("doc-2", b"0123456789").await.unwrap();

        let result = storage.read_range("doc-2", 0, 10, 3).await.unwrap();
        assert_eq!(result.bytes, b"012");
        assert_eq!(result.total_bytes, 10);
        assert!(result.truncated);
    }

    #[tokio::test]
    async fn read_range_past_eof_is_not_truncated() {
        let (_dir, storage) = temp_storage();
        storage.write_atomic("doc-3", b"abc").await.unwrap();

        let result = storage.read_range("doc-3", 0, 100, 1_000).await.unwrap();
        assert_eq!(result.bytes, b"abc");
        assert!(!result.truncated);

        let past = storage.read_range("doc-3", 10, 5, 1_000).await.unwrap();
        assert_eq!(past.bytes, Vec::<u8>::new());
        assert_eq!(past.total_bytes, 3);
        assert!(!past.truncated);
    }

    #[tokio::test]
    async fn read_range_rejects_invalid_storage_key_and_missing_file() {
        let (_dir, storage) = temp_storage();
        let err = storage.read_range("../escape", 0, 1, 1).await.unwrap_err();
        assert_eq!(err, ArtifactError::InvalidStorageKey);

        let err = storage
            .read_range("does-not-exist", 0, 1, 1)
            .await
            .unwrap_err();
        assert_eq!(err, ArtifactError::NotFound);
    }

    #[tokio::test]
    async fn delete_removes_payload_and_is_idempotent() {
        let (_dir, storage) = temp_storage();
        storage.write_atomic("doc-4", b"bytes").await.unwrap();
        storage.delete("doc-4").await.unwrap();
        assert!(!storage.root().join("doc-4").exists());
        // Deleting again is not an error.
        storage.delete("doc-4").await.unwrap();
    }

    #[tokio::test]
    async fn stream_to_copies_full_payload() {
        let (_dir, storage) = temp_storage();
        storage.write_atomic("doc-5", b"stream me").await.unwrap();

        let dest_dir = tempfile::tempdir().unwrap();
        let dest_path = dest_dir.path().join("out.bin");
        let mut dest = tokio::fs::File::create(&dest_path).await.unwrap();
        let copied = storage.stream_to("doc-5", &mut dest).await.unwrap();
        dest.flush().await.unwrap();
        assert_eq!(copied, 9);

        let contents = tokio::fs::read(&dest_path).await.unwrap();
        assert_eq!(contents, b"stream me");
    }
}
