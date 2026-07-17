//! `edit` — exact-string replacement within a worktree file, with a diff.

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::file_reference::{
    normalize_resolved_path, resolve_pinned_workspace_reference, resolve_workspace_reference,
};
use crate::harness::native::tool_contract::{NormalizedInput, ToolError, ToolInputCtx};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use similar::TextDiff;
use std::path::PathBuf;

pub struct Edit;

fn input_context(ctx: &ToolCtx) -> ToolInputCtx<'_> {
    ToolInputCtx {
        work_dir: &ctx.work_dir,
        attachments_dir: None,
        extra_skill_dirs: &[],
    }
}

fn normalize_edit_input(
    ctx: &ToolInputCtx<'_>,
    input: Value,
) -> Result<NormalizedInput, ToolError> {
    let path = input
        .get("path")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::caller("invalid_path_reference", "File path is required"))?;
    let target = resolve_workspace_reference(ctx, path)?;
    normalize_resolved_path(input, &target)
}

fn prepare_edit_execution(ctx: &ToolCtx, input: Value) -> Result<(Value, PathBuf), ToolError> {
    let input_ctx = input_context(ctx);
    if let Some(target) = ctx.pinned_file_reference.as_ref() {
        return resolve_pinned_workspace_reference(&input_ctx, target)
            .map(|resolved| (input, resolved));
    }

    let normalized = normalize_edit_input(&input_ctx, input)?;
    let target = normalized
        .pinned_file_reference()
        .expect("edit normalization pins its selected target");
    let resolved = resolve_pinned_workspace_reference(&input_ctx, target)?;
    Ok((normalized.value, resolved))
}

/// Build a literal pattern that permits bare-LF input to match either LF or
/// CRLF. Explicit CRLF input remains a literal CRLF sequence.
fn newline_tolerant_pattern(text: &str) -> Regex {
    let mut pattern = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' if chars.peek() == Some(&'\n') => {
                chars.next();
                pattern.push_str(r"\r\n");
            }
            '\n' => pattern.push_str(r"\r?\n"),
            _ => pattern.push_str(&regex::escape(&ch.to_string())),
        }
    }
    Regex::new(&pattern).expect("escaped text is valid regex")
}

fn replacement_for_file(text: &str, content: &str) -> String {
    if !content.contains("\r\n") {
        return text.to_string();
    }

    let mut normalized = String::new();
    let mut chars = text.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' && chars.peek() == Some(&'\n') {
            chars.next();
            normalized.push_str("\r\n");
        } else if ch == '\n' {
            normalized.push_str("\r\n");
        } else {
            normalized.push(ch);
        }
    }
    normalized
}

fn replace_matches(pattern: &Regex, content: &str, replacement: &str, replace_all: bool) -> String {
    let mut updated = String::with_capacity(content.len() + replacement.len());
    let mut cursor = 0;
    for matched in pattern.find_iter(content) {
        updated.push_str(&content[cursor..matched.start()]);
        updated.push_str(replacement);
        cursor = matched.end();
        if !replace_all {
            break;
        }
    }
    updated.push_str(&content[cursor..]);
    updated
}
#[async_trait]
impl Tool for Edit {
    fn name(&self) -> &str {
        "edit"
    }
    fn description(&self) -> &str {
        "Replace a literal string in a file relative to the working directory. A bare LF in `old_string` matches LF or CRLF. \
         By default `old_string` must occur exactly once; set `replace_all` to \
         replace every occurrence. Returns a unified diff of the change."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "File path relative to the working directory."},
                "old_string": {"type": "string", "description": "Exact text to replace."},
                "new_string": {"type": "string", "description": "Replacement text."},
                "replace_all": {"type": "boolean", "description": "Replace all occurrences (default false)."}
            },
            "required": ["path", "old_string", "new_string"]
        })
    }
    fn kind(&self) -> &'static str {
        "edit"
    }
    fn normalize_input(
        &self,
        ctx: &ToolInputCtx<'_>,
        input: Value,
    ) -> Result<NormalizedInput, ToolError> {
        normalize_edit_input(ctx, input)
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("edit", format!("edit {path}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let (input, resolved_path) = match prepare_edit_execution(ctx, input) {
            Ok(prepared) => prepared,
            Err(error) => return Ok(ToolOutput::from_error(error)),
        };
        let path = input
            .get("path")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("edit: `path` is required"))?;
        let old = input
            .get("old_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("edit: `old_string` is required"))?;
        let new = input
            .get("new_string")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("edit: `new_string` is required"))?;
        let replace_all = input
            .get("replace_all")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let content = match tokio::fs::read_to_string(&resolved_path).await {
            Ok(c) => c,
            Err(e) => return Ok(ToolOutput::error(format!("edit: {path}: {e}"))),
        };
        let pattern = newline_tolerant_pattern(old);
        let count = pattern.find_iter(&content).count();
        if count == 0 {
            return Ok(ToolOutput::error(format!(
                "edit: `old_string` not found in {path}"
            )));
        }
        if count > 1 && !replace_all {
            return Ok(ToolOutput::error(format!(
                "edit: `old_string` occurs {count} times in {path}; make it unique or set replace_all"
            )));
        }
        let replacement = replacement_for_file(new, &content);
        let updated = replace_matches(&pattern, &content, &replacement, replace_all);
        if let Err(e) = tokio::fs::write(&resolved_path, &updated).await {
            return Ok(ToolOutput::error(format!("edit: {path}: {e}")));
        }
        let diff = TextDiff::from_lines(&content, &updated)
            .unified_diff()
            .header(path, path)
            .to_string();
        let fmt_note = match crate::harness::native::format::maybe_format(&resolved_path).await {
            Some(fmt) => format!(" (formatted with {fmt})"),
            None => String::new(),
        };
        Ok(ToolOutput::ok(truncate(
            &format!("edited {path}{fmt_note}\n{diff}"),
            &ctx.caps,
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;
    use crate::harness::native::file_reference::{
        FileReference, FileReferenceInterpretation, FileReferenceRoot, ResolvedFileTarget,
    };
    use crate::harness::native::tool_contract::ToolInputCtx;

    #[tokio::test]
    async fn replaces_unique_string_and_returns_diff() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "hello world\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({"path": "f.txt", "old_string": "world", "new_string": "rust"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "hello rust\n"
        );
        assert!(out.for_model.contains("-hello world"));
        assert!(out.for_model.contains("+hello rust"));
    }

    #[tokio::test]
    async fn replaces_line_feed_input_in_crlf_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "before\r\nold\r\nafter\r\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({
                    "path": "f.txt",
                    "old_string": "before\nold\nafter\n",
                    "new_string": "before\nnew\nafter\n"
                }),
            )
            .await
            .unwrap();

        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "before\r\nnew\r\nafter\r\n"
        );
    }

    #[tokio::test]
    async fn preserves_crlf_when_replacing_a_single_line_token() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "before old after\r\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({
                    "path": "f.txt",
                    "old_string": "old",
                    "new_string": "first\nsecond"
                }),
            )
            .await
            .unwrap();

        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "before first\r\nsecond after\r\n"
        );
    }

    #[tokio::test]
    async fn explicit_crlf_old_string_does_not_match_lf_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "before\nold\nafter\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({
                    "path": "f.txt",
                    "old_string": "before\r\nold\r\nafter\r\n",
                    "new_string": "replacement"
                }),
            )
            .await
            .unwrap();

        assert!(out.is_error, "{}", out.for_model);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "before\nold\nafter\n"
        );
    }

    #[tokio::test]
    async fn preserves_literal_dollar_signs_in_replacement() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "old").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({"path": "f.txt", "old_string": "old", "new_string": "$0 and $1"}),
            )
            .await
            .unwrap();

        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "$0 and $1"
        );
    }

    #[tokio::test]
    async fn preserves_crlf_for_mixed_line_endings_in_replacement() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "old\r\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({
                    "path": "f.txt",
                    "old_string": "old",
                    "new_string": "one\r\ntwo\nthree"
                }),
            )
            .await
            .unwrap();

        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "one\r\ntwo\r\nthree\r\n"
        );
    }

    #[tokio::test]
    async fn replace_all_replaces_each_lf_old_string_match_in_crlf_file() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "old\r\nold\r\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({
                    "path": "f.txt",
                    "old_string": "old\n",
                    "new_string": "new\n",
                    "replace_all": true
                }),
            )
            .await
            .unwrap();

        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "new\r\nnew\r\n"
        );
    }

    #[tokio::test]
    async fn non_unique_match_without_replace_all_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "a a a").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({"path": "f.txt", "old_string": "a", "new_string": "b"}),
            )
            .await
            .unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("occurs 3 times"));
    }

    #[tokio::test]
    async fn missing_old_string_errors() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "abc").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Edit
            .execute(
                &ctx,
                json!({"path": "f.txt", "old_string": "zzz", "new_string": "y"}),
            )
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[tokio::test]
    async fn location_metadata_never_selects_an_edit_occurrence() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f.txt"), "old\nold\n").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let input_ctx = ToolInputCtx {
            work_dir: &ctx.work_dir,
            attachments_dir: None,
            extra_skill_dirs: &[],
        };

        let normalized = Edit
            .normalize_input(
                &input_ctx,
                json!({
                    "path": "f.txt:2:1",
                    "old_string": "old",
                    "new_string": "new"
                }),
            )
            .unwrap();
        assert_eq!(normalized.value["path"], "f.txt");
        assert!(normalized.value.get("occurrence").is_none());
        let metadata = serde_json::to_value(normalized.metadata()).unwrap();
        assert_eq!(metadata[0]["value"]["line"], 2);
        assert_eq!(metadata[0]["value"]["column"], 1);

        let out = Edit.execute(&ctx, normalized.value).await.unwrap();
        assert!(out.is_error);
        assert!(out.for_model.contains("occurs 2 times"));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            "old\nold\n"
        );
    }

    #[tokio::test]
    async fn workspace_skills_files_are_edited_by_relative_and_pinned_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let skill_like_dir = dir.path().join("skills/demo");
        std::fs::create_dir_all(&skill_like_dir).unwrap();
        let relative_file = skill_like_dir.join("relative.txt");
        let absolute_file = skill_like_dir.join("absolute.txt");
        std::fs::write(&relative_file, "old relative\n").unwrap();
        std::fs::write(&absolute_file, "old absolute\n").unwrap();
        let mut ctx = ctx_at(dir.path()).await;

        let relative = Edit
            .execute(
                &ctx,
                json!({
                    "path": "skills/demo/relative.txt",
                    "old_string": "old relative",
                    "new_string": "new relative"
                }),
            )
            .await
            .unwrap();
        assert!(!relative.is_error, "{}", relative.for_model);
        assert_eq!(
            std::fs::read_to_string(&relative_file).unwrap(),
            "new relative\n"
        );

        let normalized = Edit
            .normalize_input(
                &input_context(&ctx),
                json!({
                    "path": absolute_file.to_string_lossy(),
                    "old_string": "old absolute",
                    "new_string": "new absolute"
                }),
            )
            .unwrap();
        ctx.pinned_file_reference = normalized.pinned_file_reference().cloned();
        let absolute = Edit.execute(&ctx, normalized.value).await.unwrap();
        assert!(!absolute.is_error, "{}", absolute.for_model);
        assert_eq!(
            std::fs::read_to_string(&absolute_file).unwrap(),
            "new absolute\n"
        );
    }

    #[tokio::test]
    async fn disappeared_pinned_literal_never_switches_to_source_edit() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes"), "old\n").unwrap();
        let mut ctx = ctx_at(dir.path()).await;
        let target = ResolvedFileTarget {
            reference: FileReference {
                input_path: "notes:12".to_string(),
                path: "notes:12".to_string(),
                line: None,
                column: None,
            },
            interpretation: FileReferenceInterpretation::LiteralPath,
            root: FileReferenceRoot::Workspace,
            resolved_path: dir.path().join("notes:12"),
            logical_path: "notes:12".to_string(),
            exists: true,
        };
        let normalized = normalize_resolved_path(
            json!({
                "path": "notes:12",
                "old_string": "old",
                "new_string": "new"
            }),
            &target,
        )
        .unwrap();
        ctx.pinned_file_reference = normalized.pinned_file_reference().cloned();

        let out = Edit.execute(&ctx, normalized.value).await.unwrap();
        assert!(out.is_error);
        assert_eq!(
            out.structured_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("file_reference_changed")
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes")).unwrap(),
            "old\n"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pinned_source_edit_ignores_literal_candidate_appearing_later() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes"), "old\n").unwrap();
        let mut ctx = ctx_at(dir.path()).await;
        let normalized = Edit
            .normalize_input(
                &input_context(&ctx),
                json!({
                    "path": "notes:12",
                    "old_string": "old",
                    "new_string": "new"
                }),
            )
            .unwrap();
        ctx.pinned_file_reference = normalized.pinned_file_reference().cloned();
        std::fs::write(dir.path().join("notes:12"), "alternate\n").unwrap();

        let out = Edit.execute(&ctx, normalized.value).await.unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes")).unwrap(),
            "new\n"
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("notes:12")).unwrap(),
            "alternate\n"
        );
    }
}
