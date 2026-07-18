//! Session artifact listing: the read-only view over a session's originated
//! and shared-in artifacts (`crate::artifacts`). No fetch (byte read) and no
//! retention/expiry handling here — this module only maps
//! `Store::artifacts_for_session` rows to the wire DTO.

use super::{ok, params, ApiError};
use crate::api::types::{ArtifactFileInfo, ArtifactInfo};
use crate::artifacts::{ArtifactError, ArtifactListRow, ArtifactStatus};
use crate::serve::ApiState;
use base64::Engine as _;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &["list_session_artifacts", "fetch_artifact"];

#[derive(Deserialize)]
struct SessionPk {
    session_pk: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct FetchArtifactP {
    session_pk: String,
    artifact_id: String,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    match method {
        "list_session_artifacts" => {
            let a: SessionPk = params(p)?;
            ok(list_session_artifacts(state, &a.session_pk).await?)
        }
        "fetch_artifact" => {
            let a: FetchArtifactP = params(p)?;
            ok(fetch_artifact(state, &a.session_pk, &a.artifact_id).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

async fn list_session_artifacts(
    state: &ApiState,
    session_pk: &str,
) -> anyhow::Result<Vec<ArtifactInfo>> {
    let rows = state.cp.store().artifacts_for_session(session_pk).await?;
    Ok(rows.into_iter().map(artifact_info).collect())
}

async fn fetch_artifact(
    state: &ApiState,
    session_pk: &str,
    artifact_id: &str,
) -> Result<ArtifactFileInfo, ApiError> {
    let access = state
        .cp
        .store()
        .reference_for_session(artifact_id, session_pk)
        .await
        .map_err(|_| ApiError::not_found("artifact not found"))?
        .ok_or_else(|| ApiError::not_found("artifact not found"))?;
    if access.artifact.status == ArtifactStatus::Deleted {
        return Err(ApiError::not_found("artifact not found"));
    }
    let read = state
        .cp
        .artifacts()
        .read_range(&access.artifact.id, 0, Some(access.artifact.size_bytes))
        .await
        .map_err(map_read_error)?;
    if read.truncated {
        return Err(ApiError::conflict(
            "artifact exceeds the download read limit",
        ));
    }
    Ok(ArtifactFileInfo {
        name: access.artifact.name,
        content_type: access.artifact.content_type,
        data_base64: base64::engine::general_purpose::STANDARD.encode(read.bytes),
    })
}

fn map_read_error(error: ArtifactError) -> ApiError {
    match error {
        ArtifactError::NotFound | ArtifactError::Deleted | ArtifactError::AccessDenied => {
            ApiError::not_found("artifact not found")
        }
        _ => ApiError {
            status: 500,
            message: "artifact download failed".into(),
        },
    }
}

fn artifact_info(row: ArtifactListRow) -> ArtifactInfo {
    let ArtifactListRow {
        artifact,
        reference,
    } = row;
    ArtifactInfo {
        id: artifact.id,
        source_session_pk: artifact.source_session_pk,
        reference_id: reference.as_ref().map(|r| r.id.clone()),
        shared_from_session_pk: reference.as_ref().map(|r| r.shared_from_session_pk.clone()),
        parent_reference_id: reference.and_then(|r| r.parent_reference_id),
        status: artifact.status.as_db().to_string(),
        name: artifact.name,
        content_type: artifact.content_type,
        size_bytes: artifact.size_bytes,
        creator: artifact.creator.as_db().to_string(),
        created_at: artifact.created_at,
        sha256: artifact.sha256,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{dispatch, tests_support::state};
    use crate::artifacts::{ArtifactCreator, ArtifactRecord, ArtifactReference, ArtifactStatus};
    use serde_json::json;

    fn sample_artifact(id: &str, source_session_pk: &str) -> ArtifactRecord {
        ArtifactRecord {
            id: id.into(),
            source_session_pk: source_session_pk.into(),
            source_message_seq: Some(1),
            source_run_id: Some("run-1".into()),
            creator: ArtifactCreator::Agent,
            creator_id: Some("ada".into()),
            name: "report.md".into(),
            description: Some("summary".into()),
            content_type: Some("text/markdown".into()),
            size_bytes: 42,
            sha256: "deadbeef".into(),
            storage_key: format!("{id}/report.md"),
            status: ArtifactStatus::Available,
            created_at: 1_700_000_000_000,
            deleted_at: None,
        }
    }

    fn sample_reference(
        id: &str,
        artifact_id: &str,
        target_session_pk: &str,
        shared_from_session_pk: &str,
    ) -> ArtifactReference {
        ArtifactReference {
            id: id.into(),
            artifact_id: artifact_id.into(),
            target_session_pk: target_session_pk.into(),
            shared_from_session_pk: shared_from_session_pk.into(),
            shared_by: Some("ada".into()),
            parent_reference_id: None,
            created_at: 1_700_000_000_100,
        }
    }

    #[tokio::test]
    async fn list_session_artifacts_returns_source_and_referenced_rows() {
        let s = state().await;
        let artifact = sample_artifact("art-1", "s1");
        s.cp.store().insert_artifact(&artifact).await.unwrap();
        let reference = sample_reference("ref-1", "art-1", "s2", "s1");
        s.cp.store()
            .insert_artifact_reference(&reference)
            .await
            .unwrap();

        let source_out = dispatch(&s, "list_session_artifacts", json!({"session_pk": "s1"}))
            .await
            .unwrap();
        let source_list: Vec<ArtifactInfo> = serde_json::from_value(source_out).unwrap();
        assert_eq!(source_list.len(), 1);
        assert_eq!(source_list[0].id, "art-1");
        assert_eq!(source_list[0].source_session_pk, "s1");
        assert_eq!(source_list[0].reference_id, None);
        assert_eq!(source_list[0].shared_from_session_pk, None);
        assert_eq!(source_list[0].parent_reference_id, None);
        assert_eq!(source_list[0].status, "available");
        assert_eq!(source_list[0].name, "report.md");
        assert_eq!(
            source_list[0].content_type.as_deref(),
            Some("text/markdown")
        );
        assert_eq!(source_list[0].size_bytes, 42);
        assert_eq!(source_list[0].creator, "agent");
        assert_eq!(source_list[0].created_at, 1_700_000_000_000);
        assert_eq!(source_list[0].sha256, "deadbeef");

        let ref_out = dispatch(&s, "list_session_artifacts", json!({"session_pk": "s2"}))
            .await
            .unwrap();
        let ref_list: Vec<ArtifactInfo> = serde_json::from_value(ref_out).unwrap();
        assert_eq!(ref_list.len(), 1);
        assert_eq!(ref_list[0].id, "art-1");
        assert_eq!(ref_list[0].source_session_pk, "s1");
        assert_eq!(ref_list[0].reference_id.as_deref(), Some("ref-1"));
        assert_eq!(ref_list[0].shared_from_session_pk.as_deref(), Some("s1"));
        assert_eq!(ref_list[0].parent_reference_id, None);
    }
}
