use crate::harness::native::skills::SkillRegistry;
use crate::harness::native::tool_contract::{
    FileReferenceMetadata, NormalizedInput, ToolError, ToolErrorStrategy, ToolFieldError,
    ToolInputCtx, ToolMetadataEntry,
};
use crate::harness::native::tools::jail;
use serde_json::Value;
use std::path::{Component, Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileReference {
    pub input_path: String,
    pub path: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileReferenceInterpretation {
    Plain,
    LiteralPath,
    SourceReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyntacticFileCandidate {
    pub reference: FileReference,
    pub interpretation: FileReferenceInterpretation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum FileReferenceRoot {
    SkillDirectory,
    Workspace,
    Attachments,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedFileTarget {
    pub reference: FileReference,
    pub interpretation: FileReferenceInterpretation,
    pub root: FileReferenceRoot,
    pub resolved_path: PathBuf,
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CandidateProbeResult {
    NotApplicable,
    Rejected,
    Missing(PathBuf),
    Existing(PathBuf),
}

pub trait CandidateProbe {
    fn probe(&self, root: FileReferenceRoot, reference: &FileReference) -> CandidateProbeResult;
}

type ParsedSuffix<'a> = (&'a str, u32, Option<u32>);

fn invalid_path_reference() -> ToolError {
    ToolError::caller("invalid_path_reference", "File path reference is invalid")
        .with_strategy(ToolErrorStrategy::ReviseInput)
        .with_field_error(ToolFieldError::new(
            "path",
            "invalid_path_reference",
            "Invalid field value",
        ))
}

fn positive_location(value: &str) -> Result<u32, ToolError> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid_path_reference());
    }
    value
        .parse::<u32>()
        .ok()
        .filter(|value| *value > 0)
        .ok_or_else(invalid_path_reference)
}

fn is_bare_windows_drive(path: &str) -> bool {
    let bytes = path.as_bytes();
    bytes.len() == 2 && bytes[0].is_ascii_alphabetic() && bytes[1] == b':'
}

fn looks_signed_numeric(value: &str) -> bool {
    value
        .strip_prefix(['-', '+'])
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

fn has_interior_numeric_delimiter(input: &str) -> bool {
    let parts = input.split(':').collect::<Vec<_>>();
    parts.len() > 2
        && parts[1..parts.len() - 1].iter().any(|part| {
            (!part.is_empty() && part.bytes().all(|byte| byte.is_ascii_digit()))
                || looks_signed_numeric(part)
        })
}

fn source_candidates(
    input: &str,
    path: &str,
    line: u32,
    column: Option<u32>,
) -> Result<Vec<SyntacticFileCandidate>, ToolError> {
    if path.is_empty() || path.ends_with(':') || is_bare_windows_drive(path) {
        return Err(invalid_path_reference());
    }
    Ok(vec![
        SyntacticFileCandidate {
            reference: FileReference {
                input_path: input.to_string(),
                path: input.to_string(),
                line: None,
                column: None,
            },
            interpretation: FileReferenceInterpretation::LiteralPath,
        },
        SyntacticFileCandidate {
            reference: FileReference {
                input_path: input.to_string(),
                path: path.to_string(),
                line: Some(line),
                column,
            },
            interpretation: FileReferenceInterpretation::SourceReference,
        },
    ])
}

pub fn parse_file_references(input: &str) -> Result<Vec<SyntacticFileCandidate>, ToolError> {
    if input.is_empty() || is_bare_windows_drive(input) {
        return Err(invalid_path_reference());
    }

    if let Some(prefix) = input.strip_prefix(':') {
        let (line, path) = prefix.split_once(':').ok_or_else(invalid_path_reference)?;
        let line = positive_location(line)?;
        if path.is_empty() || suffix_location(path)?.is_some() {
            return Err(invalid_path_reference());
        }
        return source_candidates(input, path, line, None);
    }

    if let Some((path, line, column)) = suffix_location(input)? {
        return source_candidates(input, path, line, column);
    }

    if input.ends_with(':') || looks_signed_numeric(input.rsplit(':').next().unwrap_or(input)) {
        return Err(invalid_path_reference());
    }
    if has_interior_numeric_delimiter(input) {
        return Err(invalid_path_reference());
    }

    Ok(vec![SyntacticFileCandidate {
        reference: FileReference {
            input_path: input.to_string(),
            path: input.to_string(),
            line: None,
            column: None,
        },
        interpretation: FileReferenceInterpretation::Plain,
    }])
}

fn suffix_location(input: &str) -> Result<Option<ParsedSuffix<'_>>, ToolError> {
    let Some((left, last)) = input.rsplit_once(':') else {
        return Ok(None);
    };
    if last.is_empty() {
        return Err(invalid_path_reference());
    }
    if looks_signed_numeric(last) {
        return Err(invalid_path_reference());
    }
    if !last.bytes().all(|byte| byte.is_ascii_digit()) {
        return Ok(None);
    }
    let final_location = positive_location(last)?;

    if let Some((path, possible_line)) = left.rsplit_once(':') {
        if looks_signed_numeric(possible_line) {
            return Err(invalid_path_reference());
        }
        if !possible_line.is_empty() && possible_line.bytes().all(|byte| byte.is_ascii_digit()) {
            let line = positive_location(possible_line)?;
            if suffix_location(path)?.is_some() {
                return Err(invalid_path_reference());
            }
            return Ok(Some((path, line, Some(final_location))));
        }
    }

    Ok(Some((left, final_location, None)))
}

pub fn resolve_candidates(
    candidates: &[SyntacticFileCandidate],
    roots: &[FileReferenceRoot],
    probe: &dyn CandidateProbe,
) -> Result<ResolvedFileTarget, ToolError> {
    if candidates.is_empty() || roots.is_empty() {
        return Err(invalid_path_reference());
    }

    let mut missing = Vec::new();
    for root in roots {
        let mut existing = Vec::new();
        for (index, candidate) in candidates.iter().enumerate() {
            match probe.probe(*root, &candidate.reference) {
                CandidateProbeResult::Existing(path) => existing.push((index, path)),
                CandidateProbeResult::Missing(path) => missing.push((index, *root, path)),
                CandidateProbeResult::NotApplicable | CandidateProbeResult::Rejected => {}
            }
        }
        if existing.len() > 1 {
            let mut error = invalid_path_reference();
            for (index, _) in existing {
                error = error.with_candidate(match candidates[index].interpretation {
                    FileReferenceInterpretation::SourceReference => "source_reference",
                    FileReferenceInterpretation::Plain
                    | FileReferenceInterpretation::LiteralPath => "literal_path",
                });
            }
            return Err(error);
        }
        if let Some((index, resolved_path)) = existing.pop() {
            let candidate = &candidates[index];
            return Ok(ResolvedFileTarget {
                reference: candidate.reference.clone(),
                interpretation: candidate.interpretation,
                root: *root,
                resolved_path,
                exists: true,
            });
        }
    }

    let preferred = candidates
        .iter()
        .position(|candidate| {
            candidate.interpretation == FileReferenceInterpretation::SourceReference
        })
        .unwrap_or(0);
    if let Some((_, root, resolved_path)) = missing
        .into_iter()
        .find(|(index, _, _)| *index == preferred)
    {
        let candidate = &candidates[preferred];
        return Ok(ResolvedFileTarget {
            reference: candidate.reference.clone(),
            interpretation: candidate.interpretation,
            root,
            resolved_path,
            exists: false,
        });
    }

    Err(invalid_path_reference())
}

struct FilesystemProbe<'a> {
    context: &'a ToolInputCtx<'a>,
}

impl FilesystemProbe<'_> {
    fn result(path: anyhow::Result<PathBuf>) -> CandidateProbeResult {
        match path {
            Ok(path) if path.exists() => CandidateProbeResult::Existing(path),
            Ok(path) => CandidateProbeResult::Missing(path),
            Err(_) => CandidateProbeResult::Rejected,
        }
    }

    fn skill_path(&self, logical_path: &str) -> CandidateProbeResult {
        let mut components = Path::new(logical_path).components();
        match components.next() {
            Some(Component::Normal(first)) if first.to_str() == Some("skills") => {}
            _ => return CandidateProbeResult::NotApplicable,
        }
        let Some(Component::Normal(name)) = components.next() else {
            return CandidateProbeResult::Rejected;
        };
        let Some(name) = name.to_str() else {
            return CandidateProbeResult::Rejected;
        };
        let relative = components.as_path();
        let Some(relative) = relative.to_str().filter(|path| !path.is_empty()) else {
            return CandidateProbeResult::Rejected;
        };
        let registry =
            SkillRegistry::load_with(self.context.work_dir, self.context.extra_skill_dirs);
        let Some(skill) = registry.get(name) else {
            return CandidateProbeResult::Rejected;
        };
        Self::result(jail(&skill.dir, relative))
    }
}

impl CandidateProbe for FilesystemProbe<'_> {
    fn probe(&self, root: FileReferenceRoot, reference: &FileReference) -> CandidateProbeResult {
        let is_skill_path = Path::new(&reference.path).components().next().is_some_and(
            |first| matches!(first, Component::Normal(value) if value.to_str() == Some("skills")),
        );
        match root {
            FileReferenceRoot::SkillDirectory => self.skill_path(&reference.path),
            FileReferenceRoot::Workspace if !is_skill_path => {
                Self::result(jail(self.context.work_dir, &reference.path))
            }
            FileReferenceRoot::Attachments if !is_skill_path => self
                .context
                .attachments_dir
                .map(|root| Self::result(jail(root, &reference.path)))
                .unwrap_or(CandidateProbeResult::NotApplicable),
            FileReferenceRoot::Workspace | FileReferenceRoot::Attachments => {
                CandidateProbeResult::NotApplicable
            }
        }
    }
}

pub fn resolve_read_reference(
    context: &ToolInputCtx<'_>,
    input: &str,
) -> Result<ResolvedFileTarget, ToolError> {
    let candidates = parse_file_references(input)?;
    resolve_candidates(
        &candidates,
        &[
            FileReferenceRoot::SkillDirectory,
            FileReferenceRoot::Workspace,
            FileReferenceRoot::Attachments,
        ],
        &FilesystemProbe { context },
    )
}

pub fn resolve_workspace_reference(
    context: &ToolInputCtx<'_>,
    input: &str,
) -> Result<ResolvedFileTarget, ToolError> {
    let candidates = parse_file_references(input)?;
    resolve_candidates(
        &candidates,
        &[FileReferenceRoot::Workspace],
        &FilesystemProbe { context },
    )
}

pub fn normalize_resolved_path(
    mut input: Value,
    target: &ResolvedFileTarget,
) -> Result<NormalizedInput, ToolError> {
    let Some(object) = input.as_object_mut() else {
        return Err(invalid_path_reference());
    };
    let normalized = target.reference.input_path != target.reference.path;
    object.insert(
        "path".to_string(),
        Value::String(target.reference.path.clone()),
    );
    let metadata = FileReferenceMetadata::new(
        &target.reference.input_path,
        &target.reference.path,
        target.reference.line,
        target.reference.column,
        normalized,
    );
    let normalized_input = if normalized {
        NormalizedInput::changed(input)
    } else {
        NormalizedInput::unchanged(input)
    };
    normalized_input.with_metadata(ToolMetadataEntry::FileReference(metadata))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::native::tool_contract::ToolInputCtx;
    use std::collections::BTreeSet;

    #[test]
    fn parses_plain_suffix_windows_and_observed_prefix_forms() {
        let cases = [
            ("notes.md", "notes.md", None, None),
            ("notes.md:12", "notes.md", Some(12), None),
            ("notes.md:12:4", "notes.md", Some(12), Some(4)),
            (r"C:\repo\notes.md", r"C:\repo\notes.md", None, None),
            (r"C:\repo\notes.md:12", r"C:\repo\notes.md", Some(12), None),
            (
                r"C:\repo\notes.md:12:4",
                r"C:\repo\notes.md",
                Some(12),
                Some(4),
            ),
            (
                ":2:apps/cockpit/src/store-native.test.ts",
                "apps/cockpit/src/store-native.test.ts",
                Some(2),
                None,
            ),
        ];

        for (input, expected_path, line, column) in cases {
            let candidates = parse_file_references(input).unwrap();
            let source = candidates
                .iter()
                .find(|candidate| candidate.reference.line.is_some())
                .or_else(|| candidates.first())
                .unwrap();
            assert_eq!(source.reference.input_path, input);
            assert_eq!(source.reference.path, expected_path);
            assert_eq!(source.reference.line, line);
            assert_eq!(source.reference.column, column);
            if line.is_some() {
                assert_eq!(candidates.len(), 2, "{input}");
                assert_eq!(candidates[0].reference.path, input);
                assert_eq!(candidates[0].reference.line, None);
            } else {
                assert_eq!(candidates.len(), 1, "{input}");
            }
        }
    }

    #[test]
    fn rejects_invalid_locations_and_conflicting_delimiters() {
        for input in [
            "",
            "notes:0",
            "notes:1:0",
            ":0:notes",
            ":2:",
            "C:",
            "notes:4294967296",
            "notes:1:4294967296",
            "notes:1:2:3",
            ":2:notes:3",
            "notes::2",
            ":x:notes",
            "notes:-1",
        ] {
            let error = parse_file_references(input).unwrap_err();
            assert_eq!(error.code, "invalid_path_reference", "{input}");
        }
    }

    #[test]
    fn drive_colon_is_never_a_location_separator() {
        let plain = parse_file_references(r"D:\code\app.rs").unwrap();
        assert_eq!(plain.len(), 1);
        assert_eq!(plain[0].reference.path, r"D:\code\app.rs");
        assert_eq!(plain[0].reference.line, None);

        let located = parse_file_references(r"D:\code\app.rs:27:9").unwrap();
        assert_eq!(located[1].reference.path, r"D:\code\app.rs");
        assert_eq!(located[1].reference.line, Some(27));
        assert_eq!(located[1].reference.column, Some(9));
    }

    struct SetProbe {
        existing: BTreeSet<(FileReferenceRoot, String)>,
    }

    impl CandidateProbe for SetProbe {
        fn probe(
            &self,
            root: FileReferenceRoot,
            reference: &FileReference,
        ) -> CandidateProbeResult {
            let resolved = std::path::PathBuf::from(&reference.path);
            if self.existing.contains(&(root, reference.path.clone())) {
                CandidateProbeResult::Existing(resolved)
            } else {
                CandidateProbeResult::Missing(resolved)
            }
        }
    }

    #[test]
    fn injected_probe_rejects_literal_and_source_ambiguity() {
        let candidates = parse_file_references("notes:12").unwrap();
        let probe = SetProbe {
            existing: [
                (FileReferenceRoot::Workspace, "notes:12".to_string()),
                (FileReferenceRoot::Workspace, "notes".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let error =
            resolve_candidates(&candidates, &[FileReferenceRoot::Workspace], &probe).unwrap_err();

        assert_eq!(error.code, "invalid_path_reference");
        assert_eq!(
            error.candidates.as_ref(),
            &["literal_path".to_string(), "source_reference".to_string()]
        );
    }

    #[test]
    fn missing_location_prefers_source_and_keeps_literal_advisory() {
        for input in ["missing.rs:8", ":8:missing.rs"] {
            let candidates = parse_file_references(input).unwrap();
            let resolved = resolve_candidates(
                &candidates,
                &[FileReferenceRoot::Workspace],
                &SetProbe {
                    existing: BTreeSet::new(),
                },
            )
            .unwrap();

            assert_eq!(resolved.reference.path, "missing.rs");
            assert_eq!(resolved.reference.line, Some(8));
            assert!(!resolved.exists);
        }
    }

    #[test]
    fn resolver_honors_root_precedence() {
        let candidates = parse_file_references("notes:12").unwrap();
        let probe = SetProbe {
            existing: [
                (FileReferenceRoot::Workspace, "notes".to_string()),
                (FileReferenceRoot::Attachments, "notes:12".to_string()),
            ]
            .into_iter()
            .collect(),
        };
        let resolved = resolve_candidates(
            &candidates,
            &[
                FileReferenceRoot::SkillDirectory,
                FileReferenceRoot::Workspace,
                FileReferenceRoot::Attachments,
            ],
            &probe,
        )
        .unwrap();

        assert_eq!(resolved.root, FileReferenceRoot::Workspace);
        assert_eq!(resolved.reference.path, "notes");
    }

    #[test]
    fn real_read_resolution_preserves_skill_worktree_attachment_precedence() {
        let work = tempfile::tempdir().unwrap();
        let attachments = tempfile::tempdir().unwrap();
        let skill_dir = work.path().join(".ryuzi/skills/demo");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: demo\ndescription: Demo\n---\nBody",
        )
        .unwrap();
        std::fs::write(skill_dir.join("notes"), "skill").unwrap();
        std::fs::create_dir_all(work.path().join("skills/demo")).unwrap();
        std::fs::write(work.path().join("skills/demo/notes"), "worktree shadow").unwrap();
        std::fs::write(work.path().join("notes"), "worktree").unwrap();
        std::fs::write(attachments.path().join("notes"), "attachment").unwrap();
        let ctx = ToolInputCtx {
            work_dir: work.path(),
            attachments_dir: Some(attachments.path()),
            extra_skill_dirs: &[],
        };

        let skill = resolve_read_reference(&ctx, "skills/demo/notes:2").unwrap();
        assert_eq!(skill.root, FileReferenceRoot::SkillDirectory);
        assert_eq!(
            skill.resolved_path,
            skill_dir.join("notes").canonicalize().unwrap()
        );

        let workspace = resolve_read_reference(&ctx, "notes:2").unwrap();
        assert_eq!(workspace.root, FileReferenceRoot::Workspace);
        assert_eq!(
            workspace.resolved_path,
            work.path().join("notes").canonicalize().unwrap()
        );

        std::fs::remove_file(work.path().join("notes")).unwrap();
        let attachment = resolve_read_reference(&ctx, "notes:2").unwrap();
        assert_eq!(attachment.root, FileReferenceRoot::Attachments);
        assert_eq!(
            attachment.resolved_path,
            attachments.path().join("notes").canonicalize().unwrap()
        );
    }

    #[test]
    fn write_glob_and_grep_keep_literal_colon_inputs() {
        use crate::harness::native::tools::{glob::Glob, grep::Grep, write::Write, Tool};
        use serde_json::json;

        let dir = tempfile::tempdir().unwrap();
        let ctx = ToolInputCtx {
            work_dir: dir.path(),
            attachments_dir: None,
            extra_skill_dirs: &[],
        };
        for (tool, input) in [
            (
                &Write as &dyn Tool,
                json!({"path": "notes:12", "content": "literal"}),
            ),
            (&Glob as &dyn Tool, json!({"pattern": "notes:12"})),
            (&Grep as &dyn Tool, json!({"pattern": "notes:12"})),
        ] {
            let normalized = tool.normalize_input(&ctx, input.clone()).unwrap();
            assert_eq!(normalized.value, input);
            assert!(!normalized.normalized);
            assert!(normalized.metadata().is_empty());
        }
    }

    #[cfg(unix)]
    #[test]
    fn real_unix_files_with_literal_and_source_names_are_ambiguous() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("notes"), "source").unwrap();
        std::fs::write(dir.path().join("notes:12"), "literal").unwrap();
        let ctx = ToolInputCtx {
            work_dir: dir.path(),
            attachments_dir: None,
            extra_skill_dirs: &[],
        };

        let error = resolve_workspace_reference(&ctx, "notes:12").unwrap_err();
        assert_eq!(error.code, "invalid_path_reference");
    }

    #[cfg(unix)]
    #[test]
    fn real_probe_rejects_symlink_escape() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        std::fs::write(outside.path().join("secret"), "nope").unwrap();
        symlink(outside.path(), root.path().join("escape")).unwrap();
        let ctx = ToolInputCtx {
            work_dir: root.path(),
            attachments_dir: None,
            extra_skill_dirs: &[],
        };

        let error = resolve_workspace_reference(&ctx, "escape/secret:2").unwrap_err();
        assert_eq!(error.code, "invalid_path_reference");
    }

    #[cfg(windows)]
    #[test]
    fn windows_drive_location_resolves_without_splitting_the_drive() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("notes.txt");
        std::fs::write(&file, "notes").unwrap();
        let input = format!("{}:2", file.display());
        let ctx = ToolInputCtx {
            work_dir: dir.path(),
            attachments_dir: None,
            extra_skill_dirs: &[],
        };

        let resolved = resolve_workspace_reference(&ctx, &input).unwrap();
        assert_eq!(resolved.reference.path, file.to_string_lossy());
        assert_eq!(resolved.reference.line, Some(2));
    }
}
