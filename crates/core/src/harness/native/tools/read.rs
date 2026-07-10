//! `read` — read a text file within the session worktree.

use super::{jail, truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use async_trait::async_trait;
use serde_json::{json, Value};

/// 2 MiB read cap, matching the ACP fs handler and Cockpit's `read_file`.
const MAX_READ_BYTES: u64 = 2 * 1024 * 1024;

/// Images at or under this size come back as a vision block; larger ones get
/// an honest size error (provider hard limits sit at ~5 MB anyway).
pub(crate) const IMAGE_READ_MAX_BYTES: u64 = 5 * 1024 * 1024;

fn image_media_type_for_ext(path: &str) -> Option<&'static str> {
    let ext = std::path::Path::new(path)
        .extension()
        .and_then(|e| e.to_str())?
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        "gif" => Some("image/gif"),
        "webp" => Some("image/webp"),
        _ => None,
    }
}

pub struct Read;

#[async_trait]
impl Tool for Read {
    fn name(&self) -> &str {
        "read"
    }
    fn description(&self) -> &str {
        "Read a UTF-8 text file or image (png/jpg/gif/webp) from the working \
         directory or an attachment path from the conversation. Text supports \
         optional line offset/limit; lines are prefixed with 1-based numbers."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to the working directory."},
                "offset": {"type": "integer", "description": "1-based line to start from (default 1)."},
                "limit": {"type": "integer", "description": "Maximum number of lines to read (default 2000)."}
            },
            "required": ["path"]
        })
    }
    fn kind(&self) -> &'static str {
        "read"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("read", format!("read {path}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("read: `path` is required"))?;
        // Primary root: the worktree. Fallback root: the session's attachment
        // folder — the manifest hands the model ABSOLUTE paths there, which
        // the worktree jail (correctly) rejects.
        let resolved = match jail(&ctx.work_dir, path) {
            Ok(p) => p,
            Err(primary_err) => {
                match ctx.attachments_dir.as_deref().and_then(|root| jail(root, path).ok()) {
                    Some(p) => p,
                    None => return Ok(ToolOutput::error(primary_err.to_string())),
                }
            }
        };
        let meta = match tokio::fs::metadata(&resolved).await {
            Ok(m) => m,
            Err(e) => return Ok(ToolOutput::error(format!("read: {path}: {e}"))),
        };
        if let Some(media_type) = image_media_type_for_ext(path) {
            if meta.len() > IMAGE_READ_MAX_BYTES {
                return Ok(ToolOutput::error(format!(
                    "read: {path} is {:.1} MB — too large to attach (5 MB limit). \
                     Ask the user for a smaller version.",
                    meta.len() as f64 / (1024.0 * 1024.0)
                )));
            }
            use base64::Engine as _;
            let bytes = match tokio::fs::read(&resolved).await {
                Ok(b) => b,
                Err(e) => return Ok(ToolOutput::error(format!("read: {path}: {e}"))),
            };
            let data = base64::engine::general_purpose::STANDARD.encode(bytes);
            return Ok(ToolOutput {
                for_model: format!("[image {path} ({media_type}, {} bytes) attached]", meta.len()),
                model_blocks: Some(vec![json!({
                    "type": "image",
                    "source": { "type": "base64", "media_type": media_type, "data": data }
                })]),
                display: None,
                is_error: false,
            });
        }
        if meta.len() > MAX_READ_BYTES {
            return Ok(ToolOutput::error(format!(
                "read: {path} is {} bytes, over the {MAX_READ_BYTES} byte cap",
                meta.len()
            )));
        }
        let content = match tokio::fs::read_to_string(&resolved).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::error(format!("read: {path}: {e}"))),
        };
        let offset = input
            .get("offset")
            .and_then(|v| v.as_u64())
            .unwrap_or(1)
            .max(1) as usize;
        let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(2000) as usize;
        let numbered = content
            .lines()
            .enumerate()
            .skip(offset - 1)
            .take(limit)
            .map(|(i, line)| format!("{:>6}\t{}", i + 1, line))
            .collect::<Vec<_>>()
            .join("\n");
        Ok(ToolOutput::ok(truncate(&numbered, &ctx.caps)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn reads_numbered_lines_and_honors_offset_limit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a\nb\nc\nd\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "f.txt", "offset": 2, "limit": 2}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("     2\tb"));
        assert!(out.for_model.contains("     3\tc"));
        assert!(!out.for_model.contains("\ta\n") && !out.for_model.contains("     1\ta"));
    }

    #[tokio::test]
    async fn missing_file_is_a_tool_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "nope.txt"}))
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn escape_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "../secret"}))
            .await
            .unwrap();
        assert!(out.is_error);
    }

    /// A 1x1 PNG (89 50 4E 47 … minimal valid file is unnecessary — the tool
    /// keys off the extension and returns bytes; use a tiny fake payload).
    const PNG_BYTES: &[u8] = &[0x89, b'P', b'N', b'G', 0, 1, 2, 3];

    #[tokio::test]
    async fn attachments_dir_paths_are_readable() {
        let work = tempfile::tempdir().unwrap();
        let attach = tempfile::tempdir().unwrap();
        std::fs::write(attach.path().join("notes.txt"), "hello\n").unwrap();
        let mut ctx = ctx_at(work.path()).await;
        ctx.attachments_dir = Some(attach.path().to_path_buf());
        let abs = attach.path().join("notes.txt");
        let out = Read
            .execute(&ctx, json!({"path": abs.to_string_lossy()}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("hello"));
    }

    #[tokio::test]
    async fn non_attachment_escapes_stay_rejected() {
        let work = tempfile::tempdir().unwrap();
        let attach = tempfile::tempdir().unwrap();
        let elsewhere = tempfile::tempdir().unwrap();
        std::fs::write(elsewhere.path().join("secret"), "x").unwrap();
        let mut ctx = ctx_at(work.path()).await;
        ctx.attachments_dir = Some(attach.path().to_path_buf());
        let abs = elsewhere.path().join("secret");
        let out = Read
            .execute(&ctx, json!({"path": abs.to_string_lossy()}))
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn image_read_returns_an_image_block() {
        let work = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("shot.png"), PNG_BYTES).unwrap();
        let ctx = ctx_at(work.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "shot.png"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        let blocks = out.model_blocks.expect("image read must carry blocks");
        assert_eq!(blocks[0]["type"], "image");
        assert_eq!(blocks[0]["source"]["media_type"], "image/png");
        assert!(!blocks[0]["source"]["data"].as_str().unwrap().is_empty());
    }

    #[tokio::test]
    async fn oversized_image_is_an_honest_error() {
        let work = tempfile::tempdir().unwrap();
        let big = vec![0u8; (super::IMAGE_READ_MAX_BYTES + 1) as usize];
        std::fs::write(work.path().join("big.png"), big).unwrap();
        let ctx = ctx_at(work.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "big.png"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("too large"), "{}", out.for_model);
    }
}
