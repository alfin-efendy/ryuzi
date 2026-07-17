//! `ls` — list a directory within the session worktree.

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::file_reference::{
    normalize_resolved_path, path_unavailable, preflight_file_target,
    recheck_preflight_file_target, resolve_pinned_workspace_reference, resolve_workspace_reference,
    ExpectedFileKind, PinnedFileTarget,
};
use crate::harness::native::tool_contract::{
    NormalizedInput, PreflightMeta, ToolError, ToolInputCtx,
};
use async_trait::async_trait;
use serde_json::{json, Value};
use std::path::PathBuf;

pub struct Ls;

fn input_context(ctx: &ToolCtx) -> ToolInputCtx<'_> {
    ToolInputCtx {
        work_dir: &ctx.work_dir,
        attachments_dir: None,
        extra_skill_dirs: &[],
    }
}

fn normalize_ls_input(ctx: &ToolInputCtx<'_>, input: Value) -> Result<NormalizedInput, ToolError> {
    let path_was_omitted = input.get("path").is_none();
    let path = match input.get("path") {
        Some(path) => path
            .as_str()
            .ok_or_else(|| ToolError::caller("invalid_path_reference", "Invalid file path"))?
            .to_string(),
        None => ".".to_string(),
    };
    let target = resolve_workspace_reference(ctx, &path)?;
    let mut normalized = normalize_resolved_path(input, &target)?;
    if path_was_omitted {
        normalized.normalized = true;
    }
    Ok(normalized)
}

async fn prepare_ls_execution(ctx: &ToolCtx, input: Value) -> Result<(Value, PathBuf), ToolError> {
    let input_ctx = input_context(ctx);
    if let Some(target) = ctx.preflight_file_target.as_ref() {
        let resolved = recheck_preflight_file_target(&input_ctx, target).await?;
        return Ok((input, resolved.resolved_path));
    }
    if let Some(target) = ctx.pinned_file_reference.as_ref() {
        return resolve_pinned_workspace_reference(&input_ctx, target)
            .map(|resolved| (input, resolved));
    }

    let normalized = normalize_ls_input(&input_ctx, input)?;
    let target = normalized
        .pinned_file_reference()
        .expect("ls normalization pins its selected target");
    let resolved = resolve_pinned_workspace_reference(&input_ctx, target)?;
    Ok((normalized.value, resolved))
}

#[async_trait]
impl Tool for Ls {
    fn name(&self) -> &str {
        "ls"
    }
    fn description(&self) -> &str {
        "List the entries of a directory relative to the working directory. \
         Directories are suffixed with `/`."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "path": {"type": "string", "description": "Directory path relative to the working directory (default `.`)."}
            }
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
        normalize_ls_input(ctx, input)
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
            preflight_file_target(ctx, target, ExpectedFileKind::Directory).await?,
        )
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        PermissionSpec::new("read", format!("list {path}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let (input, resolved_path) = match prepare_ls_execution(ctx, input).await {
            Ok(prepared) => prepared,
            Err(error) => return Ok(ToolOutput::from_error(error)),
        };
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
        let mut rd = match tokio::fs::read_dir(&resolved_path).await {
            Ok(r) => r,
            Err(error) if ctx.preflight_file_target.is_some() => {
                return Ok(ToolOutput::from_error(path_unavailable(&error)));
            }
            Err(error) => return Ok(ToolOutput::error(format!("ls: {path}: {error}"))),
        };
        let mut entries: Vec<String> = Vec::new();
        loop {
            let entry = match rd.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(error) if ctx.preflight_file_target.is_some() => {
                    return Ok(ToolOutput::from_error(path_unavailable(&error)));
                }
                Err(_) => break,
            };
            let name = entry.file_name().to_string_lossy().to_string();
            let is_dir = entry.file_type().await.map(|t| t.is_dir()).unwrap_or(false);
            entries.push(if is_dir { format!("{name}/") } else { name });
        }
        entries.sort();
        Ok(ToolOutput::ok(truncate(&entries.join("\n"), &ctx.caps)))
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;
    use crate::harness::native::tool_contract::ToolInputCtx;

    #[tokio::test]
    async fn lists_files_and_dirs_sorted() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("b.txt"), "").unwrap();
        std::fs::create_dir(dir.path().join("adir")).unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = Ls.execute(&ctx, json!({"path": "."})).await.unwrap();
        assert!(!out.is_error);
        assert_eq!(out.for_model, "adir/\nb.txt");
    }

    #[tokio::test]
    async fn omitted_path_defaults_to_workspace_root_without_panicking() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("root.txt"), "").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let input_ctx = ToolInputCtx {
            work_dir: &ctx.work_dir,
            attachments_dir: None,
            extra_skill_dirs: &[],
        };

        let normalized = Ls.normalize_input(&input_ctx, json!({})).unwrap();
        assert_eq!(normalized.value, json!({"path": "."}));
        assert!(normalized.normalized);
        assert!(normalized.pinned_file_reference().is_some());

        let out = Ls.execute(&ctx, json!({})).await.unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(out.for_model, "root.txt");
    }

    #[tokio::test]
    async fn workspace_skills_directory_is_listed_by_relative_and_pinned_absolute_path() {
        let dir = tempfile::tempdir().unwrap();
        let skill_like_dir = dir.path().join("skills/demo");
        std::fs::create_dir_all(&skill_like_dir).unwrap();
        std::fs::write(skill_like_dir.join("item.txt"), "").unwrap();
        let mut ctx = ctx_at(dir.path()).await;

        let relative = Ls
            .execute(&ctx, json!({"path": "skills/demo"}))
            .await
            .unwrap();
        assert!(!relative.is_error, "{}", relative.for_model);
        assert_eq!(relative.for_model, "item.txt");

        let normalized = Ls
            .normalize_input(
                &input_context(&ctx),
                json!({"path": skill_like_dir.to_string_lossy()}),
            )
            .unwrap();
        ctx.pinned_file_reference = normalized.pinned_file_reference().cloned();
        let absolute = Ls.execute(&ctx, normalized.value).await.unwrap();
        assert!(!absolute.is_error, "{}", absolute.for_model);
        assert_eq!(absolute.for_model, "item.txt");
    }

    #[tokio::test]
    async fn location_is_metadata_only_and_lists_the_selected_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("sub")).unwrap();
        std::fs::write(dir.path().join("sub/item.txt"), "").unwrap();
        let ctx = ctx_at(dir.path()).await;
        let input_ctx = ToolInputCtx {
            work_dir: &ctx.work_dir,
            attachments_dir: None,
            extra_skill_dirs: &[],
        };

        let normalized = Ls
            .normalize_input(&input_ctx, json!({"path": "sub:7:3"}))
            .unwrap();
        assert_eq!(normalized.value, json!({"path": "sub"}));
        let metadata = serde_json::to_value(normalized.metadata()).unwrap();
        assert_eq!(metadata[0]["value"]["line"], 7);
        assert_eq!(metadata[0]["value"]["column"], 3);

        let out = Ls.execute(&ctx, normalized.value).await.unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(out.for_model, "item.txt");
    }

    #[tokio::test]
    async fn v2_preflight_rejects_file_with_stable_windows_safe_error() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join(".git"), "file").unwrap();
        let ctx = ctx_at(root.path()).await;
        let normalized = Ls
            .normalize_input(&input_context(&ctx), json!({"path": ".git"}))
            .unwrap();
        let pin = normalized.pinned_file_reference().unwrap();

        let error = Ls
            .preflight(&input_context(&ctx), &normalized.value, Some(pin))
            .await
            .unwrap_err();
        let serialized = serde_json::to_string(&error).unwrap();

        assert_eq!(error.code, "expected_directory");
        let details = error.details.as_ref().unwrap();
        assert_eq!(details["actual_kind"], "file");
        assert_eq!(details["suggested_tool"], "read");
        assert!(!serialized.contains("os error 267"));
        assert!(!serialized.contains(&root.path().to_string_lossy().to_string()));
    }

    #[tokio::test]
    async fn prepared_ls_target_detects_existence_race() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("folder")).unwrap();
        let mut ctx = ctx_at(root.path()).await;
        let normalized = Ls
            .normalize_input(&input_context(&ctx), json!({"path": "folder"}))
            .unwrap();
        let pin = normalized.pinned_file_reference().unwrap().clone();
        let preflight = Ls
            .preflight(&input_context(&ctx), &normalized.value, Some(&pin))
            .await
            .unwrap();
        ctx.pinned_file_reference = Some(pin);
        ctx.preflight_file_target = preflight.prepared_file_target().cloned();

        std::fs::remove_dir(root.path().join("folder")).unwrap();
        let out = Ls.execute(&ctx, normalized.value).await.unwrap();

        assert!(out.is_error);
        assert_eq!(
            out.structured_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("file_precondition_changed")
        );
    }
}
