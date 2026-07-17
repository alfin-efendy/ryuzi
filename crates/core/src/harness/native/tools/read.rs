//! `read` — read a text file within the session worktree.

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::file_reference::{
    normalize_resolved_path, path_unavailable, preflight_file_target,
    recheck_preflight_file_target, resolve_pinned_read_reference, resolve_read_reference,
    ExpectedFileKind, PinnedFileTarget,
};
use crate::harness::native::tool_contract::{
    NormalizedInput, PreflightMeta, ToolError, ToolErrorStrategy, ToolFieldError, ToolInputCtx,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

/// 2 MiB read cap, matching Cockpit's `read_file` command.
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

/// Sniff the image container format from magic bytes, independent of the
/// filename extension. `None` for unrecognized (or too-short) content — e.g.
/// a git-lfs pointer file (plain ASCII text) or an SVG renamed `.png`.
///
/// This exists because trusting the extension alone lets bad bytes become a
/// durable "image" content block in the provider ledger: the provider 400s
/// on it, and since the block is already appended, EVERY subsequent request
/// (including compaction) replays it and 400s again — the session is bricked.
fn sniff_image_media_type(bytes: &[u8]) -> Option<&'static str> {
    if bytes.len() >= 4 && &bytes[0..4] == b"\x89PNG" {
        return Some("image/png");
    }
    if bytes.len() >= 2 && &bytes[0..2] == b"\xFF\xD8" {
        return Some("image/jpeg");
    }
    if bytes.len() >= 4 && &bytes[0..4] == b"GIF8" {
        return Some("image/gif");
    }
    if bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        return Some("image/webp");
    }
    None
}

fn input_context(ctx: &ToolCtx) -> ToolInputCtx<'_> {
    ToolInputCtx {
        work_dir: &ctx.work_dir,
        attachments_dir: ctx.attachments_dir.as_deref(),
        extra_skill_dirs: &ctx.extra_skill_dirs,
    }
}

fn read_io_error(ctx: &ToolCtx, path: &str, error: &std::io::Error) -> ToolOutput {
    if ctx.preflight_file_target.is_some() {
        ToolOutput::from_error(path_unavailable(error))
    } else {
        ToolOutput::error(format!("read: {path}: {error}"))
    }
}

fn normalize_read_input(
    ctx: &ToolInputCtx<'_>,
    input: Value,
) -> Result<NormalizedInput, ToolError> {
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::caller("invalid_path_reference", "File path is required"))?;
    let target = resolve_read_reference(ctx, path)?;
    let line = target.reference.line;
    let mut normalized = normalize_resolved_path(input, &target)?;
    if let Some(line) = line {
        let object = normalized.value.as_object_mut().expect("validated object");
        match object.get("offset") {
            Some(offset) if offset.as_u64() == Some(u64::from(line)) => {}
            Some(_) => {
                return Err(ToolError::caller(
                    "conflicting_file_location",
                    "Path line and explicit offset conflict",
                )
                .with_strategy(ToolErrorStrategy::ReviseInput)
                .with_field_error(ToolFieldError::new(
                    "path",
                    "conflicting_file_location",
                    "Invalid field value",
                )))
            }
            None => {
                object.insert("offset".to_string(), Value::from(line));
                normalized.normalized = true;
            }
        }
    }
    Ok(normalized)
}

async fn prepare_read_execution(
    ctx: &ToolCtx,
    input: Value,
) -> Result<(Value, PathBuf), ToolError> {
    let input_ctx = input_context(ctx);
    if let Some(target) = ctx.preflight_file_target.as_ref() {
        let resolved = recheck_preflight_file_target(&input_ctx, target).await?;
        return Ok((input, resolved.resolved_path));
    }
    if let Some(target) = ctx.pinned_file_reference.as_ref() {
        return resolve_pinned_read_reference(&input_ctx, target).map(|resolved| (input, resolved));
    }

    let normalized = normalize_read_input(&input_ctx, input)?;
    let target = normalized
        .pinned_file_reference()
        .expect("read normalization pins its selected target");
    let resolved = resolve_pinned_read_reference(&input_ctx, target)?;
    Ok((normalized.value, resolved))
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
         optional line offset/limit; lines are prefixed with 1-based numbers. \
         `skills/<skill-name>/<relative-path>` reads a companion file bundled \
         alongside a discovered skill's SKILL.md."
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
    fn normalize_input(
        &self,
        ctx: &ToolInputCtx<'_>,
        input: Value,
    ) -> Result<NormalizedInput, ToolError> {
        normalize_read_input(ctx, input)
    }
    async fn preflight(
        &self,
        ctx: &ToolInputCtx<'_>,
        _input: &Value,
        pinned_file_reference: Option<&PinnedFileTarget>,
    ) -> Result<PreflightMeta, ToolError> {
        let target = pinned_file_reference.ok_or_else(|| {
            ToolError::precondition("invalid_path_reference", "File target is not pinned")
        })?;
        PreflightMeta::default().with_prepared_file_target(
            preflight_file_target(ctx, target, ExpectedFileKind::File).await?,
        )
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("read", format!("read {path}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let (input, resolved_path) = match prepare_read_execution(ctx, input).await {
            Ok(prepared) => prepared,
            Err(error) => return Ok(ToolOutput::from_error(error)),
        };
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("read: `path` is required"))?;
        finish_read(ctx, path, &resolved_path, &input).await
    }
}

/// Finish reading `resolved` (already jailed/verified) — shared by both the
/// worktree/attachment path and the skill-companion path.
async fn finish_read(
    ctx: &ToolCtx,
    path: &str,
    resolved: &Path,
    input: &Value,
) -> anyhow::Result<ToolOutput> {
    let meta = match tokio::fs::metadata(resolved).await {
        Ok(m) => m,
        Err(error) => return Ok(read_io_error(ctx, path, &error)),
    };
    if let Some(media_type) = image_media_type_for_ext(path) {
        if meta.len() > IMAGE_READ_MAX_BYTES {
            return Ok(ToolOutput::error(format!(
                "read: {path} is {:.1} MB — too large to attach (5 MB limit). \
                 Ask the user for a smaller version.",
                meta.len() as f64 / (1024.0 * 1024.0)
            )));
        }
        if meta.len() == 0 {
            return Ok(ToolOutput::error(format!(
                "read: {path} is empty — not a valid image"
            )));
        }
        use base64::Engine as _;
        let bytes = match tokio::fs::read(resolved).await {
            Ok(b) => b,
            Err(error) => return Ok(read_io_error(ctx, path, &error)),
        };
        match sniff_image_media_type(&bytes) {
            Some(sniffed) if sniffed == media_type => {}
            _ => {
                return Ok(ToolOutput::error(format!(
                    "read: {path} does not contain valid {media_type} data — \
                     possibly a git-lfs pointer; try 'git lfs pull'"
                )));
            }
        }
        let data = base64::engine::general_purpose::STANDARD.encode(bytes);
        return Ok(ToolOutput {
            for_model: format!(
                "[image {path} ({media_type}, {} bytes) attached]",
                meta.len()
            ),
            model_blocks: Some(vec![json!({
                "type": "image",
                "source": { "type": "base64", "media_type": media_type, "data": data }
            })]),
            display: None,
            is_error: false,
            structured_error: None,
        });
    }
    if meta.len() > MAX_READ_BYTES {
        return Ok(ToolOutput::error(format!(
            "read: {path} is {} bytes, over the {MAX_READ_BYTES} byte cap",
            meta.len()
        )));
    }
    let content = match tokio::fs::read_to_string(resolved).await {
        Ok(c) => c,
        Err(error) => return Ok(read_io_error(ctx, path, &error)),
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

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;
    use crate::harness::native::tool_contract::ToolInputCtx;

    fn input_ctx(ctx: &ToolCtx) -> ToolInputCtx<'_> {
        ToolInputCtx {
            work_dir: &ctx.work_dir,
            attachments_dir: ctx.attachments_dir.as_deref(),
            extra_skill_dirs: &ctx.extra_skill_dirs,
        }
    }

    #[tokio::test]
    async fn source_line_fills_missing_offset_and_preserves_bounded_metadata() {
        let work = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("notes.txt"), "one\ntwo\n").unwrap();
        let ctx = ctx_at(work.path()).await;

        let normalized = Read
            .normalize_input(&input_ctx(&ctx), json!({"path": "notes.txt:2"}))
            .unwrap();

        assert_eq!(normalized.value["path"], "notes.txt");
        assert_eq!(normalized.value["offset"], 2);
        assert!(normalized.normalized);
        let metadata = serde_json::to_value(normalized.metadata()).unwrap();
        assert_eq!(metadata[0]["kind"], "file_reference");
        assert_eq!(metadata[0]["value"]["input_path"], "notes.txt:2");
        assert_eq!(metadata[0]["value"]["resolved_path"], "notes.txt");
        assert_eq!(metadata[0]["value"]["line"], 2);
        assert_eq!(metadata[0]["value"]["column"], Value::Null);
        assert_eq!(metadata[0]["value"]["normalized"], true);
    }

    #[tokio::test]
    async fn source_line_accepts_equal_offset_and_rejects_conflict() {
        let work = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("notes.txt"), "one\ntwo\n").unwrap();
        let ctx = ctx_at(work.path()).await;

        let equal = Read
            .normalize_input(
                &input_ctx(&ctx),
                json!({"path": "notes.txt:2", "offset": 2}),
            )
            .unwrap();
        assert_eq!(equal.value["path"], "notes.txt");
        assert_eq!(equal.value["offset"], 2);

        let error = Read
            .normalize_input(
                &input_ctx(&ctx),
                json!({"path": "notes.txt:2", "offset": 1}),
            )
            .unwrap_err();
        assert_eq!(error.code, "conflicting_file_location");
    }

    #[tokio::test]
    async fn relative_attachment_path_uses_attachment_after_workspace_miss() {
        let work = tempfile::tempdir().unwrap();
        let attach = tempfile::tempdir().unwrap();
        std::fs::write(attach.path().join("notes.txt"), "attachment\n").unwrap();
        let mut ctx = ctx_at(work.path()).await;
        ctx.attachments_dir = Some(attach.path().to_path_buf());

        let normalized = Read
            .normalize_input(&input_ctx(&ctx), json!({"path": "notes.txt:1"}))
            .unwrap();
        let out = Read.execute(&ctx, normalized.value).await.unwrap();

        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("attachment"));
    }

    #[tokio::test]
    async fn pinned_workspace_read_never_falls_back_to_same_named_attachment() {
        let work = tempfile::tempdir().unwrap();
        let attach = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("notes.txt"), "workspace\n").unwrap();
        let mut ctx = ctx_at(work.path()).await;
        ctx.attachments_dir = Some(attach.path().to_path_buf());

        let normalized = Read
            .normalize_input(&input_ctx(&ctx), json!({"path": "notes.txt"}))
            .unwrap();
        ctx.pinned_file_reference = normalized.pinned_file_reference().cloned();

        std::fs::remove_file(work.path().join("notes.txt")).unwrap();
        std::fs::write(attach.path().join("notes.txt"), "attachment-secret\n").unwrap();

        let out = Read.execute(&ctx, normalized.value).await.unwrap();
        assert!(out.is_error);
        assert_eq!(
            out.structured_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("file_reference_changed")
        );
        assert!(!out.for_model.contains("attachment-secret"));
    }

    #[tokio::test]
    async fn absolute_paths_become_logical_in_canonical_input_and_metadata() {
        let work = tempfile::tempdir().unwrap();
        let file = work.path().join("notes.txt");
        std::fs::write(&file, "notes\n").unwrap();
        let ctx = ctx_at(work.path()).await;
        for (input, logical_input, expected_offset) in [
            (file.display().to_string(), "notes.txt", None),
            (format!("{}:1", file.display()), "notes.txt:1", Some(1)),
            (format!(":1:{}", file.display()), ":1:notes.txt", Some(1)),
        ] {
            let normalized = Read
                .normalize_input(&input_ctx(&ctx), json!({"path": input}))
                .unwrap();
            assert_eq!(normalized.value["path"], "notes.txt");
            assert_eq!(
                normalized.value.get("offset").and_then(Value::as_u64),
                expected_offset
            );
            let metadata_value = serde_json::to_value(normalized.metadata()).unwrap();
            assert_eq!(metadata_value[0]["value"]["input_path"], logical_input);
            assert_eq!(metadata_value[0]["value"]["resolved_path"], "notes.txt");
            let metadata = metadata_value.to_string();

            assert!(!metadata.contains(&work.path().to_string_lossy().to_string()));
            assert!(!metadata.contains("os error"));
            assert!(!metadata.contains("absolute_path"));
        }
    }

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
    async fn v2_preflight_rejects_directory_before_execution() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("folder")).unwrap();
        let ctx = ctx_at(root.path()).await;
        let normalized = Read
            .normalize_input(&input_ctx(&ctx), json!({"path": "folder"}))
            .unwrap();
        let pin = normalized.pinned_file_reference().unwrap();

        let error = Read
            .preflight(&input_ctx(&ctx), &normalized.value, Some(pin))
            .await
            .unwrap_err();

        assert_eq!(error.code, "expected_file");
        let details = error.details.as_ref().unwrap();
        assert_eq!(details["actual_kind"], "directory");
        assert_eq!(details["suggested_tool"], "ls");
    }

    #[tokio::test]
    async fn prepared_read_target_detects_kind_race_without_attachment_fallback() {
        let root = tempfile::tempdir().unwrap();
        let attachments = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("same"), "workspace").unwrap();
        std::fs::write(attachments.path().join("same"), "attachment-secret").unwrap();
        let mut ctx = ctx_at(root.path()).await;
        ctx.attachments_dir = Some(attachments.path().to_path_buf());
        let normalized = Read
            .normalize_input(&input_ctx(&ctx), json!({"path": "same"}))
            .unwrap();
        let pin = normalized.pinned_file_reference().unwrap().clone();
        let preflight = Read
            .preflight(&input_ctx(&ctx), &normalized.value, Some(&pin))
            .await
            .unwrap();
        ctx.pinned_file_reference = Some(pin);
        ctx.preflight_file_target = preflight.prepared_file_target().cloned();

        std::fs::remove_file(root.path().join("same")).unwrap();
        std::fs::create_dir(root.path().join("same")).unwrap();
        let out = Read.execute(&ctx, normalized.value).await.unwrap();

        assert!(out.is_error);
        assert_eq!(
            out.structured_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("file_precondition_changed")
        );
        assert!(!out.for_model.contains("attachment-secret"));
    }

    #[tokio::test]
    async fn v2_post_preflight_io_error_is_stable_and_redacted() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("invalid.txt"), [0xff, 0xfe]).unwrap();
        let mut ctx = ctx_at(root.path()).await;
        let normalized = Read
            .normalize_input(&input_ctx(&ctx), json!({"path": "invalid.txt"}))
            .unwrap();
        let pin = normalized.pinned_file_reference().unwrap().clone();
        let preflight = Read
            .preflight(&input_ctx(&ctx), &normalized.value, Some(&pin))
            .await
            .unwrap();
        ctx.pinned_file_reference = Some(pin);
        ctx.preflight_file_target = preflight.prepared_file_target().cloned();

        let out = Read.execute(&ctx, normalized.value).await.unwrap();

        assert!(out.is_error);
        assert_eq!(
            out.structured_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("path_unavailable")
        );
        assert!(!out.for_model.contains("UTF-8"));
        assert!(!out.for_model.contains("os error"));
        assert!(!out
            .for_model
            .contains(&root.path().to_string_lossy().to_string()));
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

    /// A real PNG magic header (the 8-byte signature) plus a few filler
    /// bytes — enough to pass the magic-byte sniff without a full valid
    /// PNG stream.
    const PNG_BYTES: &[u8] = &[0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, 1, 2, 3];

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
    async fn pinned_absolute_attachment_under_skills_is_readable() {
        let work = tempfile::tempdir().unwrap();
        let attach = tempfile::tempdir().unwrap();
        let file = attach.path().join("skills/demo/notes.txt");
        std::fs::create_dir_all(file.parent().unwrap()).unwrap();
        std::fs::write(&file, "attachment under skills\n").unwrap();
        let mut ctx = ctx_at(work.path()).await;
        ctx.attachments_dir = Some(attach.path().to_path_buf());

        let normalized = Read
            .normalize_input(&input_ctx(&ctx), json!({"path": file.to_string_lossy()}))
            .unwrap();
        ctx.pinned_file_reference = normalized.pinned_file_reference().cloned();
        let out = Read.execute(&ctx, normalized.value).await.unwrap();

        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("attachment under skills"));
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
    async fn empty_image_file_is_an_honest_error() {
        let work = tempfile::tempdir().unwrap();
        std::fs::write(work.path().join("empty.png"), []).unwrap();
        let ctx = ctx_at(work.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "empty.png"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("empty"), "{}", out.for_model);
    }

    #[tokio::test]
    async fn lfs_pointer_masquerading_as_png_is_rejected() {
        // A git-lfs pointer file is plain ASCII text — nothing like a PNG's
        // magic bytes — but an extension-only check would wave it through
        // and hand the provider a 400 that poisons the whole session ledger.
        let work = tempfile::tempdir().unwrap();
        let pointer = b"version https://git-lfs.github.com/spec/v1\n\
                         oid sha256:0000000000000000000000000000000000000000000000000000000000000000\n\
                         size 130\n";
        std::fs::write(work.path().join("logo.png"), pointer).unwrap();
        let ctx = ctx_at(work.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "logo.png"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(
            out.for_model.contains("git-lfs") || out.for_model.contains("not"),
            "{}",
            out.for_model
        );
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

    fn write_skill(work: &std::path::Path, name: &str) -> std::path::PathBuf {
        let skill_dir = work.join(".ryuzi/skills").join(name);
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: Test skill\n---\nBody."),
        )
        .unwrap();
        skill_dir
    }

    #[tokio::test]
    async fn reads_a_companion_file_beside_a_discovered_skill() {
        let work = tempfile::tempdir().unwrap();
        let skill_dir = write_skill(work.path(), "mytool");
        std::fs::create_dir_all(skill_dir.join("assets")).unwrap();
        std::fs::write(skill_dir.join("assets/notes.txt"), "companion body\n").unwrap();
        let ctx = ctx_at(work.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "skills/mytool/assets/notes.txt"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("companion body"));
    }

    #[tokio::test]
    async fn skill_companion_traversal_escape_is_rejected() {
        let work = tempfile::tempdir().unwrap();
        write_skill(work.path(), "mytool");
        std::fs::write(work.path().join(".ryuzi/skills/secret.txt"), "nope").unwrap();
        let ctx = ctx_at(work.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "skills/mytool/../secret.txt"}))
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn unknown_skill_virtual_path_does_not_fall_back_to_worktree() {
        // A worktree file that happens to share the virtual skill's relative
        // path must NOT be served when the named skill doesn't exist.
        let work = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(work.path().join("skills/ghost")).unwrap();
        std::fs::write(
            work.path().join("skills/ghost/notes.txt"),
            "should not read",
        )
        .unwrap();
        let ctx = ctx_at(work.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "skills/ghost/notes.txt"}))
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(!out.for_model.contains("should not read"));
    }

    #[tokio::test]
    async fn malformed_skill_virtual_path_is_an_error() {
        let work = tempfile::tempdir().unwrap();
        write_skill(work.path(), "mytool");
        let ctx = ctx_at(work.path()).await;
        // Missing a relative path component after the skill name.
        let out = Read
            .execute(&ctx, json!({"path": "skills/mytool"}))
            .await
            .unwrap();
        assert!(out.is_error);
        // Missing the skill name entirely.
        let out2 = Read.execute(&ctx, json!({"path": "skills"})).await.unwrap();
        assert!(out2.is_error);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn skill_companion_symlink_escape_is_rejected() {
        use std::os::unix::fs::symlink;
        let work = tempfile::tempdir().unwrap();
        let skill_dir = write_skill(work.path(), "mytool");
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret.txt"), "outside").unwrap();
        symlink(outside.path(), skill_dir.join("escape")).unwrap();
        let ctx = ctx_at(work.path()).await;
        let out = Read
            .execute(&ctx, json!({"path": "skills/mytool/escape/secret.txt"}))
            .await
            .unwrap();
        assert!(out.is_error);
    }
}
