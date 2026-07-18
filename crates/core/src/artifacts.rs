pub mod service;
pub mod storage;
pub mod types;

pub use service::{
    ingest_saved_attachments, ArtifactConfig, ArtifactRead, ArtifactService, CreateArtifact,
};
pub use storage::{ArtifactError, ArtifactStorage, ReadRange};
pub use types::{
    ArtifactAccessRow, ArtifactCreator, ArtifactListRow, ArtifactRecord, ArtifactReference,
    ArtifactStatus,
};

#[cfg(test)]
mod service_tdd_probe {
    //! TDD probe: written before `ArtifactService` exists, to force this
    //! module to fail to compile until the service is implemented. Removed
    //! once real service tests below cover the same ground.
    use super::*;
    use std::sync::Arc;

    #[tokio::test]
    async fn create_artifact_persists_metadata_and_payload() {
        let storage_dir = tempfile::tempdir().unwrap();
        let db_file = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(db_file.path()).await.unwrap());
        let storage = ArtifactStorage::new(storage_dir.path());
        let service = ArtifactService::new(
            store,
            storage,
            ArtifactConfig {
                max_bytes: 1_000,
                session_max_bytes: 10_000,
                read_max_bytes: 1_000,
            },
        );

        let record = service
            .create_artifact(CreateArtifact {
                session_pk: "sess-1".into(),
                source_message_seq: Some(3),
                source_run_id: Some("run-1".into()),
                creator: ArtifactCreator::Agent,
                creator_id: Some("ada".into()),
                name: "report.md".into(),
                description: Some("summary".into()),
                content_type: Some("text/markdown".into()),
                bytes: b"hello".to_vec(),
            })
            .await
            .unwrap();

        assert_eq!(record.name, "report.md");
        assert_eq!(record.size_bytes, 5);
    }
}
