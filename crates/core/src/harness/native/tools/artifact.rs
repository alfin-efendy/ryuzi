//! Native task-artifact tools.

use super::{PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use base64::Engine as _;
use serde_json::{json, Value};

fn required<'a>(input: &'a Value, key: &str) -> Result<&'a str, ToolOutput> {
    input
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ToolOutput::error(format!("{key} is required")))
}

pub struct ReadArtifact;

#[async_trait]
impl Tool for ReadArtifact {
    fn name(&self) -> &str {
        "read_artifact"
    }
    fn description(&self) -> &str {
        "Explicitly read a session artifact. Supports offset and length for chunked reads."
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"artifactId":{"type":"string"},"offset":{"type":"integer","minimum":0},"length":{"type":"integer","minimum":1}},"required":["artifactId"]})
    }
    fn kind(&self) -> &'static str {
        "read"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        PermissionSpec::new(
            "read_artifact",
            format!(
                "read artifact {}",
                input
                    .get("artifactId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ),
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let artifact_id = match required(&input, "artifactId") {
            Ok(value) => value,
            Err(out) => return Ok(out),
        };
        let offset = input.get("offset").and_then(Value::as_u64).unwrap_or(0);
        let length = input.get("length").and_then(Value::as_u64);
        let access = match ctx
            .artifacts
            .resolve_agent_access(&ctx.session_pk, artifact_id)
            .await
        {
            Ok(access) => access,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        let read = match ctx
            .artifacts
            .read_for_agent(&access.artifact.id, offset, length)
            .await
        {
            Ok(read) => read,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        let content = match String::from_utf8(read.bytes.clone()) {
            Ok(text) => json!({"encoding":"utf8","text":text}),
            Err(_) => {
                json!({"encoding":"base64","dataBase64":base64::engine::general_purpose::STANDARD.encode(read.bytes)})
            }
        };
        Ok(ToolOutput::ok(json!({
            "id":read.artifact.id,"name":read.artifact.name,"contentType":read.artifact.content_type,
            "sizeBytes":read.artifact.size_bytes,"sha256":read.artifact.sha256,
            "offset":read.offset,"totalBytes":read.total_bytes,"truncated":read.truncated,"content":content
        }).to_string()))
    }
}

pub struct WriteArtifact;

#[async_trait]
impl Tool for WriteArtifact {
    fn name(&self) -> &str {
        "write_artifact"
    }
    fn description(&self) -> &str {
        "Create a durable text or binary artifact in the active session."
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"name":{"type":"string"},"description":{"type":"string"},"contentType":{"type":"string"},"text":{"type":"string"},"dataBase64":{"type":"string"}},"required":["name"]})
    }
    fn kind(&self) -> &'static str {
        "edit"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        PermissionSpec::new(
            "write_artifact",
            format!(
                "write artifact {}",
                input.get("name").and_then(Value::as_str).unwrap_or("")
            ),
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let name = match required(&input, "name") {
            Ok(value) => value.to_string(),
            Err(out) => return Ok(out),
        };
        let text = input.get("text").and_then(Value::as_str);
        let data_base64 = input.get("dataBase64").and_then(Value::as_str);
        let bytes = match (text, data_base64) {
            (Some(text), None) => text.as_bytes().to_vec(),
            (None, Some(encoded)) => {
                match base64::engine::general_purpose::STANDARD.decode(encoded) {
                    Ok(bytes) => bytes,
                    Err(_) => return Ok(ToolOutput::error("dataBase64 is invalid")),
                }
            }
            _ => {
                return Ok(ToolOutput::error(
                    "provide exactly one of text or dataBase64",
                ))
            }
        };
        let record = match ctx
            .artifacts
            .create_artifact(crate::artifacts::CreateArtifact {
                session_pk: ctx.session_pk.clone(),
                source_message_seq: None,
                source_run_id: Some(ctx.run_id.clone()),
                creator: crate::artifacts::ArtifactCreator::Agent,
                creator_id: ctx
                    .interaction
                    .as_ref()
                    .map(|interaction| interaction.requesting_agent_id.clone()),
                name,
                description: input
                    .get("description")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                content_type: input
                    .get("contentType")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                bytes,
            })
            .await
        {
            Ok(record) => record,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        Ok(ToolOutput::ok(json!({"id":record.id,"name":record.name,"contentType":record.content_type,"sizeBytes":record.size_bytes,"sha256":record.sha256}).to_string()))
    }
}

pub struct ShareArtifact;

#[async_trait]
impl Tool for ShareArtifact {
    fn name(&self) -> &str {
        "share_artifact"
    }
    fn description(&self) -> &str {
        "Share a readable artifact as a read-only reference with another active session."
    }
    fn input_schema(&self) -> Value {
        json!({"type":"object","properties":{"artifactId":{"type":"string"},"targetSessionPk":{"type":"string"}},"required":["artifactId","targetSessionPk"]})
    }
    fn kind(&self) -> &'static str {
        "other"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        PermissionSpec::new(
            "share_artifact",
            format!(
                "share artifact {}",
                input
                    .get("artifactId")
                    .and_then(Value::as_str)
                    .unwrap_or("")
            ),
        )
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let artifact_id = match required(&input, "artifactId") {
            Ok(value) => value,
            Err(out) => return Ok(out),
        };
        let target = match required(&input, "targetSessionPk") {
            Ok(value) => value,
            Err(out) => return Ok(out),
        };
        let actor = ctx
            .interaction
            .as_ref()
            .map(|interaction| interaction.requesting_agent_id.as_str());
        let reference = match ctx
            .artifacts
            .share(&ctx.session_pk, artifact_id, target, actor)
            .await
        {
            Ok(reference) => reference,
            Err(error) => return Ok(ToolOutput::error(error.to_string())),
        };
        Ok(ToolOutput::ok(json!({"referenceId":reference.id,"artifactId":reference.artifact_id,"targetSessionPk":reference.target_session_pk,"sharedFromSessionPk":reference.shared_from_session_pk,"parentReferenceId":reference.parent_reference_id,"createdAt":reference.created_at}).to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn artifact_tools_declare_independent_permissions() {
        assert_eq!(
            ReadArtifact.permission(&json!({"artifactId":"a"})).key,
            "read_artifact"
        );
        assert_eq!(
            WriteArtifact
                .permission(&json!({"name":"a","text":"x"}))
                .key,
            "write_artifact"
        );
        assert_eq!(
            ShareArtifact
                .permission(&json!({"artifactId":"a","targetSessionPk":"s2"}))
                .key,
            "share_artifact"
        );
    }
}
