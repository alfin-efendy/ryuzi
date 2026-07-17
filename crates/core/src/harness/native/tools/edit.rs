//! `edit` — exact-string replacement within a worktree file, with a diff.

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::file_reference::{
    normalize_resolved_path, path_unavailable, preflight_file_target,
    recheck_preflight_file_target, resolve_pinned_workspace_reference, resolve_workspace_reference,
    ExpectedFileKind,
};
use crate::harness::native::tool_contract::{
    NormalizedInput, PreflightMeta, ToolError, ToolErrorStrategy, ToolInputCtx,
    MAX_TOOL_ERROR_LINE_CANDIDATES, MAX_TOOL_ERROR_LINE_PREVIEW_CHARS,
};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{json, Value};
use similar::TextDiff;
use std::path::PathBuf;

pub struct Edit;

const MIN_NO_MATCH_SIMILARITY_PERCENT: usize = 75;
const MIN_SUGGESTION_PATTERN_CHARS: usize = 8;
const MAX_SUGGESTION_PATTERNS: usize = 8;
const MAX_SUGGESTION_PATTERN_LINES_SCANNED: usize = 64;
const MAX_SUGGESTION_FILE_LINES_SCANNED: usize = 4_096;
const MAX_SUGGESTION_WORK_CELLS: usize = 250_000;

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

async fn prepare_edit_execution(
    ctx: &ToolCtx,
    input: Value,
) -> Result<(Value, PathBuf, Option<String>), ToolError> {
    let input_ctx = input_context(ctx);
    if let Some(precondition) = ctx.edit_precondition.as_ref() {
        let (resolved_path, content) = precondition.read_current(&input_ctx).await?;
        return Ok((input, resolved_path, Some(content)));
    }
    if let Some(target) = ctx.preflight_file_target.as_ref() {
        let resolved = recheck_preflight_file_target(&input_ctx, target)
            .await
            .map_err(|_| {
                ToolError::new(
                    crate::harness::native::tool_contract::ToolErrorCategory::Conflict,
                    "edit_precondition_changed",
                    "Edit precondition changed after approval",
                )
                .with_strategy(ToolErrorStrategy::ReviseInput)
            })?;
        return Ok((input, resolved.resolved_path, None));
    }
    if let Some(target) = ctx.pinned_file_reference.as_ref() {
        return resolve_pinned_workspace_reference(&input_ctx, target)
            .map(|resolved| (input, resolved, None));
    }

    let normalized = normalize_edit_input(&input_ctx, input)?;
    let target = normalized
        .pinned_file_reference()
        .expect("edit normalization pins its selected target");
    let resolved = resolve_pinned_workspace_reference(&input_ctx, target)?;
    Ok((normalized.value, resolved, None))
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

fn line_starts(content: &str) -> Vec<usize> {
    let mut starts = Vec::with_capacity(content.lines().count() + 1);
    starts.push(0);
    starts.extend(
        content
            .bytes()
            .enumerate()
            .filter_map(|(offset, byte)| (byte == b'\n').then_some(offset + 1)),
    );
    starts
}

fn line_for_offset(starts: &[usize], offset: usize) -> usize {
    starts.partition_point(|start| *start <= offset).max(1)
}

fn bounded_line_preview(content: &str, starts: &[usize], offset: usize) -> String {
    let line_index = line_for_offset(starts, offset) - 1;
    let start = starts[line_index];
    let mut end = starts
        .get(line_index + 1)
        .map_or(content.len(), |next| next.saturating_sub(1));
    if content.as_bytes().get(end.wrapping_sub(1)) == Some(&b'\r') {
        end -= 1;
    }
    content[start..end]
        .chars()
        .take(MAX_TOOL_ERROR_LINE_PREVIEW_CHARS)
        .collect()
}

fn match_candidates(content: &str, pattern: &Regex) -> Vec<(usize, String)> {
    let starts = line_starts(content);
    pattern
        .find_iter(content)
        .take(MAX_TOOL_ERROR_LINE_CANDIDATES)
        .map(|matched| {
            (
                line_for_offset(&starts, matched.start()),
                bounded_line_preview(content, &starts, matched.start()),
            )
        })
        .collect()
}

fn substring_edit_distance(
    pattern: &[char],
    text: &[char],
    stats: &mut SuggestionSearchStats,
) -> Option<usize> {
    let cells = pattern.len().saturating_mul(text.len());
    let next_work = stats.work_cells.saturating_add(cells);
    if next_work > MAX_SUGGESTION_WORK_CELLS {
        stats.exhausted = true;
        return None;
    }
    stats.work_cells = next_work;
    let mut previous = vec![0; text.len() + 1];
    let mut current = vec![0; text.len() + 1];
    for (pattern_index, pattern_char) in pattern.iter().enumerate() {
        current[0] = pattern_index + 1;
        for (text_index, text_char) in text.iter().enumerate() {
            current[text_index + 1] = (previous[text_index + 1] + 1)
                .min(current[text_index] + 1)
                .min(previous[text_index] + usize::from(pattern_char != text_char));
        }
        std::mem::swap(&mut previous, &mut current);
    }
    Some(previous.into_iter().min().unwrap_or(pattern.len()))
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuggestionPattern {
    chars: Vec<char>,
    ordinal: usize,
}

struct SimilarLine {
    line: usize,
    preview: String,
    edits: usize,
    pattern_len: usize,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct SuggestionSearchStats {
    patterns: usize,
    pattern_lines_scanned: usize,
    work_cells: usize,
    exhausted: bool,
    lines_scanned: usize,
    qualifying_candidates: usize,
    retained_peak: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SuggestionSearch {
    candidates: Vec<(usize, String)>,
    stats: SuggestionSearchStats,
}

fn suggestion_patterns(old: &str, stats: &mut SuggestionSearchStats) -> Vec<SuggestionPattern> {
    let mut patterns = Vec::<SuggestionPattern>::new();
    for (ordinal, line) in old
        .split('\n')
        .take(MAX_SUGGESTION_PATTERN_LINES_SCANNED)
        .enumerate()
    {
        stats.pattern_lines_scanned += 1;
        let chars = line
            .trim_end_matches('\r')
            .trim()
            .chars()
            .take(MAX_TOOL_ERROR_LINE_PREVIEW_CHARS)
            .collect::<Vec<_>>();
        if chars.len() < MIN_SUGGESTION_PATTERN_CHARS
            || patterns.iter().any(|pattern| pattern.chars == chars)
        {
            continue;
        }
        patterns.push(SuggestionPattern { chars, ordinal });
        patterns.sort_by(|left, right| {
            right
                .chars
                .len()
                .cmp(&left.chars.len())
                .then_with(|| left.ordinal.cmp(&right.ordinal))
        });
        patterns.truncate(MAX_SUGGESTION_PATTERNS);
    }
    stats.patterns = patterns.len();
    patterns
}

fn similar_line_order(left: &SimilarLine, right: &SimilarLine) -> std::cmp::Ordering {
    (left.edits * right.pattern_len)
        .cmp(&(right.edits * left.pattern_len))
        .then_with(|| left.line.cmp(&right.line))
}

fn retain_similar_line(
    candidates: &mut Vec<SimilarLine>,
    candidate: SimilarLine,
    stats: &mut SuggestionSearchStats,
) {
    stats.qualifying_candidates += 1;
    if candidates.len() < MAX_TOOL_ERROR_LINE_CANDIDATES {
        candidates.push(candidate);
    } else {
        let worst = candidates
            .iter()
            .enumerate()
            .max_by(|(_, left), (_, right)| similar_line_order(left, right))
            .map(|(index, _)| index)
            .expect("the bounded candidate set is non-empty");
        if similar_line_order(&candidate, &candidates[worst]).is_lt() {
            candidates[worst] = candidate;
        }
    }
    stats.retained_peak = stats.retained_peak.max(candidates.len());
}

fn no_match_candidates_with_stats(content: &str, old: &str) -> SuggestionSearch {
    let mut stats = SuggestionSearchStats::default();
    let patterns = suggestion_patterns(old, &mut stats);
    if patterns.is_empty() {
        return SuggestionSearch {
            candidates: Vec::new(),
            stats,
        };
    }

    let request_chars = patterns
        .iter()
        .map(|pattern| pattern.chars.len())
        .sum::<usize>();
    let mut candidates = Vec::with_capacity(MAX_TOOL_ERROR_LINE_CANDIDATES);
    'lines: for (line_index, line) in content
        .split_inclusive('\n')
        .take(MAX_SUGGESTION_FILE_LINES_SCANNED)
        .enumerate()
    {
        stats.lines_scanned += 1;
        let physical_line = line.strip_suffix('\n').unwrap_or(line);
        let physical_line = physical_line.strip_suffix('\r').unwrap_or(physical_line);
        let preview = physical_line
            .chars()
            .take(MAX_TOOL_ERROR_LINE_PREVIEW_CHARS)
            .collect::<String>();
        let text = preview.trim().chars().collect::<Vec<_>>();
        if text.is_empty() {
            continue;
        }
        let mut best = None::<(usize, usize)>;
        for pattern in &patterns {
            let Some(local_edits) = substring_edit_distance(&pattern.chars, &text, &mut stats)
            else {
                break 'lines;
            };
            // Missing request context counts as edits. A short exact fragment
            // therefore cannot outweigh the distinctive remainder of a
            // multi-line `old_string`.
            let score = (
                local_edits + request_chars.saturating_sub(pattern.chars.len()),
                request_chars,
            );
            if best.is_none_or(|current| (score.0 * current.1).cmp(&(current.0 * score.1)).is_lt())
            {
                best = Some(score);
            }
        }
        let Some((edits, pattern_len)) = best else {
            continue;
        };
        // At least 75% whole-request character similarity is required. Below
        // that threshold a line is advisory guessing rather than a useful retry.
        if edits * 100 <= pattern_len * (100 - MIN_NO_MATCH_SIMILARITY_PERCENT) {
            retain_similar_line(
                &mut candidates,
                SimilarLine {
                    line: line_index + 1,
                    preview,
                    edits,
                    pattern_len,
                },
                &mut stats,
            );
        }
    }
    candidates.sort_by(similar_line_order);
    let candidates = candidates
        .into_iter()
        .take(MAX_TOOL_ERROR_LINE_CANDIDATES)
        .map(|candidate| (candidate.line, candidate.preview))
        .collect();
    SuggestionSearch { candidates, stats }
}

fn no_match_candidates(content: &str, old: &str) -> Vec<(usize, String)> {
    no_match_candidates_with_stats(content, old).candidates
}

fn validate_edit_match(input: &Value, content: &str) -> Result<(), ToolError> {
    let old = input
        .get("old_string")
        .and_then(Value::as_str)
        .ok_or_else(|| ToolError::caller("invalid_arguments", "Tool arguments are invalid"))?;
    let replace_all = input
        .get("replace_all")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let pattern = newline_tolerant_pattern(old);
    let count = pattern.find_iter(content).count();
    if count == 0 {
        let mut error = ToolError::precondition("edit_match_not_found", "Edit match was not found")
            .with_strategy(ToolErrorStrategy::ReviseInput);
        for (line, preview) in no_match_candidates(content, old) {
            error = error.with_line_candidate(line, preview);
        }
        return Err(error);
    }
    if count > 1 && !replace_all {
        let mut error = ToolError::precondition("edit_ambiguous", "Edit match is ambiguous")
            .with_strategy(ToolErrorStrategy::ReviseInput)
            .with_details(json!({"match_count": count}));
        for (line, preview) in match_candidates(content, &pattern) {
            error = error.with_line_candidate(line, preview);
        }
        return Err(error);
    }
    Ok(())
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
    async fn preflight(
        &self,
        ctx: &ToolInputCtx<'_>,
        input: &Value,
        pinned_file_reference: Option<&crate::harness::native::file_reference::PinnedFileTarget>,
    ) -> Result<PreflightMeta, ToolError> {
        let target = pinned_file_reference.ok_or_else(|| {
            ToolError::precondition("invalid_path_reference", "File target is not pinned")
        })?;
        let prepared = preflight_file_target(ctx, target, ExpectedFileKind::File).await?;
        let content = tokio::fs::read_to_string(&prepared.target.resolved_path)
            .await
            .map_err(|error| path_unavailable(&error))?;
        validate_edit_match(input, &content)?;
        PreflightMeta::default()
            .with_prepared_file_target(prepared)?
            .with_edit_content_precondition(&content)
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let path = input.get("path").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("edit", format!("edit {path}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let (input, resolved_path, prepared_content) =
            match prepare_edit_execution(ctx, input).await {
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

        let content = match prepared_content {
            Some(content) => content,
            None => match tokio::fs::read_to_string(&resolved_path).await {
                Ok(content) => content,
                Err(error) => return Ok(ToolOutput::error(format!("edit: {path}: {error}"))),
            },
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
        if let Some(precondition) = ctx.edit_precondition.as_ref() {
            if let Err(error) = precondition.recheck(&input_context(ctx)).await {
                return Ok(ToolOutput::from_error(error));
            }
        }
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

    async fn edit_preflight_error(
        dir: &tempfile::TempDir,
        content: &str,
        input: Value,
    ) -> (ToolCtx, ToolError) {
        std::fs::write(dir.path().join("f.txt"), content).unwrap();
        let ctx = ctx_at(dir.path()).await;
        let normalized = Edit.normalize_input(&input_context(&ctx), input).unwrap();
        let error = Edit
            .preflight(
                &input_context(&ctx),
                &normalized.value,
                normalized.pinned_file_reference(),
            )
            .await
            .unwrap_err();
        (ctx, error)
    }

    fn serialized_line_candidates(error: &ToolError) -> Vec<Value> {
        serde_json::to_value(error).unwrap()["candidates"]
            .as_array()
            .unwrap()
            .clone()
    }

    #[tokio::test]
    async fn ambiguous_edit_returns_lines_and_does_not_mutate_or_snapshot() {
        let dir = tempfile::tempdir().unwrap();
        let before = "fn first() { target(); }\n\nfn second() { target(); }\n";
        let (ctx, error) = edit_preflight_error(
            &dir,
            before,
            json!({
                "path": "f.txt",
                "old_string": "target();",
                "new_string": "replacement();"
            }),
        )
        .await;

        assert_eq!(error.code, "edit_ambiguous");
        assert_eq!(
            serde_json::to_value(&error).unwrap()["details"]["match_count"],
            2
        );
        assert_eq!(
            serialized_line_candidates(&error)
                .iter()
                .map(|candidate| candidate["line"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 3]
        );
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            before
        );
        assert!(ctx.snapshots.lock().await.is_empty());
    }

    #[tokio::test]
    async fn ambiguous_edit_crlf_lines_and_previews_are_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let line = format!("{} target();", "é".repeat(300));
        let before = (0..7).map(|_| format!("{line}\r\n")).collect::<String>();
        let (_ctx, error) = edit_preflight_error(
            &dir,
            &before,
            json!({
                "path": "f.txt",
                "old_string": "target();",
                "new_string": "replacement();"
            }),
        )
        .await;

        assert_eq!(error.code, "edit_ambiguous");
        let candidates = serialized_line_candidates(&error);
        assert_eq!(candidates.len(), 5);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate["line"].as_u64().unwrap())
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
        assert!(candidates
            .iter()
            .all(|candidate| { candidate["preview"].as_str().unwrap().chars().count() == 240 }));
        assert_eq!(
            std::fs::read_to_string(dir.path().join("f.txt")).unwrap(),
            before
        );
    }

    #[tokio::test]
    async fn edit_no_match_candidates_are_confidence_bounded() {
        let close_dir = tempfile::tempdir().unwrap();
        let (_ctx, close_error) = edit_preflight_error(
            &close_dir,
            "let total = calculate_value();\n",
            json!({
                "path": "f.txt",
                "old_string": "let total = calculate_values();",
                "new_string": "replacement"
            }),
        )
        .await;
        assert_eq!(close_error.code, "edit_match_not_found");
        assert_eq!(serialized_line_candidates(&close_error).len(), 1);
        assert_eq!(serialized_line_candidates(&close_error)[0]["line"], 1);

        let unrelated_dir = tempfile::tempdir().unwrap();
        let (_ctx, unrelated_error) = edit_preflight_error(
            &unrelated_dir,
            "fn unrelated() { return 42; }\n",
            json!({
                "path": "f.txt",
                "old_string": "delete_all_customer_records();",
                "new_string": "replacement"
            }),
        )
        .await;
        assert_eq!(unrelated_error.code, "edit_match_not_found");
        assert!(serialized_line_candidates(&unrelated_error).is_empty());

        let common_fragment_dir = tempfile::tempdir().unwrap();
        let (_ctx, common_fragment_error) = edit_preflight_error(
            &common_fragment_dir,
            "const model = load_fixture();\n",
            json!({
                "path": "f.txt",
                "old_string": "delete_all_customer_records();\nmode\ncommit_transaction();",
                "new_string": "replacement"
            }),
        )
        .await;
        assert_eq!(common_fragment_error.code, "edit_match_not_found");
        assert!(serialized_line_candidates(&common_fragment_error).is_empty());
    }

    #[tokio::test]
    async fn prepared_edit_rechecks_digest_immediately_before_mutation() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("f.txt");
        std::fs::write(&target, "old still unique\n").unwrap();
        let mut ctx = ctx_at(dir.path()).await;
        let normalized = Edit
            .normalize_input(
                &input_context(&ctx),
                json!({
                    "path": "f.txt",
                    "old_string": "old",
                    "new_string": "new"
                }),
            )
            .unwrap();
        let preflight = Edit
            .preflight(
                &input_context(&ctx),
                &normalized.value,
                normalized.pinned_file_reference(),
            )
            .await
            .unwrap();
        ctx.preflight_file_target = preflight.prepared_file_target().cloned();
        ctx.edit_precondition = preflight.prepared_edit_precondition();

        let raced = "prefix old still unique\n";
        std::fs::write(&target, raced).unwrap();
        let out = Edit.execute(&ctx, normalized.value).await.unwrap();

        assert!(out.is_error);
        assert_eq!(
            out.structured_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("edit_precondition_changed")
        );
        assert_eq!(std::fs::read_to_string(&target).unwrap(), raced);
    }

    #[test]
    fn no_match_search_has_deterministic_pattern_and_work_budgets() {
        let old = (0..128)
            .map(|index| format!("requested-pattern-{:02}-not-present", index % 16))
            .collect::<Vec<_>>()
            .join("\n");
        let content = (0..2_000)
            .map(|index| format!("unrelated-file-line-{index:04}\n"))
            .collect::<String>();

        let first = no_match_candidates_with_stats(&content, &old);
        let second = no_match_candidates_with_stats(&content, &old);

        assert_eq!(first, second);
        assert_eq!(first.stats.patterns, MAX_SUGGESTION_PATTERNS);
        assert!(first.stats.pattern_lines_scanned <= MAX_SUGGESTION_PATTERN_LINES_SCANNED);
        assert!(first.stats.work_cells <= MAX_SUGGESTION_WORK_CELLS);
        assert!(first.stats.exhausted);
        assert!(first.stats.lines_scanned < 2_000);
        assert!(first.candidates.len() <= MAX_TOOL_ERROR_LINE_CANDIDATES);
    }

    #[test]
    fn no_match_search_retains_only_the_best_five_while_scanning() {
        let content = (0..1_000)
            .map(|_| "let total = calculate_value();\n")
            .collect::<String>();
        let search = no_match_candidates_with_stats(&content, "let total = calculate_values();");

        assert!(search.stats.qualifying_candidates > MAX_TOOL_ERROR_LINE_CANDIDATES);
        assert_eq!(search.stats.retained_peak, MAX_TOOL_ERROR_LINE_CANDIDATES);
        assert_eq!(
            search
                .candidates
                .iter()
                .map(|(line, _)| *line)
                .collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn no_match_search_bounds_empty_physical_lines_without_dp_work() {
        let content = "\n".repeat(10_000);
        let search = no_match_candidates_with_stats(&content, "a distinctive requested line");

        assert_eq!(search.stats.work_cells, 0);
        assert_eq!(search.stats.lines_scanned, 4_096);
        assert!(search.candidates.is_empty());
    }

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
