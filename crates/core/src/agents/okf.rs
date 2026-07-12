//! On-disk Knowledge Format (OKF) codec: Markdown concepts with a strict
//! YAML frontmatter header (type/title/description/RFC3339 timestamp),
//! unknown-field preservation, and safe relative-path validation for the
//! per-agent knowledge bundle.

use anyhow::{bail, Context};
use chrono::{DateTime, SecondsFormat, Utc};
use indexmap::IndexMap;
use serde_yaml::Value;

/// Generated file names that are never valid concept documents.
pub const RESERVED_FILE_NAMES: [&str; 2] = ["index.md", "log.md"];

/// Where a concept lives inside the per-agent knowledge bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KnowledgeScope {
    Global,
    User,
    Project { project_id: String },
}

/// The typed area a new concept is filed under. Each area maps to exactly
/// one bundle directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConceptArea {
    Memory(KnowledgeScope),
    Skill,
    Review,
    Journey,
    CuratorState,
    CuratorHistory,
}

impl ConceptArea {
    /// The bundle directory this area's concepts are stored in.
    pub fn directory(&self) -> anyhow::Result<String> {
        Ok(match self {
            Self::Memory(KnowledgeScope::Global) => "memory/global".into(),
            Self::Memory(KnowledgeScope::User) => "memory/user".into(),
            Self::Memory(KnowledgeScope::Project { project_id }) => {
                validate_path_component(project_id).context("invalid project id")?;
                format!("memory/projects/{project_id}")
            }
            Self::Skill => "learning/skills".into(),
            Self::Review => "learning/reviews".into(),
            Self::Journey => "learning/journey".into(),
            Self::CuratorState => "learning/curator".into(),
            Self::CuratorHistory => "learning/curator-history".into(),
        })
    }

    /// The frontmatter `type` value for concepts created in this area.
    pub fn concept_type(&self) -> &'static str {
        match self {
            Self::Memory(_) => "Memory",
            Self::Skill => "Skill",
            Self::Review => "Review",
            Self::Journey => "Journey",
            Self::CuratorState => "CuratorState",
            Self::CuratorHistory => "CuratorHistory",
        }
    }

    /// The scope recorded in frontmatter; only memory concepts carry one.
    pub fn scope(&self) -> Option<KnowledgeScope> {
        match self {
            Self::Memory(scope) => Some(scope.clone()),
            _ => None,
        }
    }
}

/// A parsed OKF concept. Unknown frontmatter fields are preserved in
/// `extensions` in their original order so round-trips are lossless.
#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeConcept {
    pub id: String,
    pub relative_path: String,
    pub concept_type: String,
    pub title: String,
    pub description: String,
    pub timestamp: DateTime<Utc>,
    pub body: String,
    pub scope: Option<KnowledgeScope>,
    pub agent_id: Option<String>,
    pub event_id: Option<String>,
    pub tags: Vec<String>,
    pub extensions: IndexMap<String, Value>,
}

/// A Markdown file inside the bundle that failed OKF parsing. The raw bytes
/// stay on disk untouched; the store surfaces it for repair or deletion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidKnowledgeConcept {
    pub relative_path: String,
    pub error: String,
    pub raw_markdown: String,
}

/// Author-facing input for creating or replacing a concept.
#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeConceptInput {
    pub area: ConceptArea,
    pub title: String,
    pub description: String,
    pub body: String,
    pub tags: Vec<String>,
    pub extensions: IndexMap<String, Value>,
}

/// Parses an OKF document. Frontmatter is split only when the first line is
/// `---` and a later line is exactly `---`; recognized fields become typed
/// values, everything else is preserved in order; the timestamp must be
/// RFC3339.
pub fn parse_concept(relative_path: &str, raw: &str) -> anyhow::Result<KnowledgeConcept> {
    let (frontmatter, body) = split_frontmatter(raw)?;
    let mut fields: IndexMap<String, Value> =
        serde_yaml::from_str(&frontmatter).context("frontmatter is not a YAML mapping")?;

    let concept_type = take_string(&mut fields, "type")?.context("missing required `type`")?;
    let title = take_string(&mut fields, "title")?.context("missing required `title`")?;
    let description =
        take_string(&mut fields, "description")?.context("missing required `description`")?;
    let raw_timestamp =
        take_string(&mut fields, "timestamp")?.context("missing required `timestamp`")?;
    let timestamp = DateTime::parse_from_rfc3339(&raw_timestamp)
        .with_context(|| format!("timestamp `{raw_timestamp}` is not RFC3339"))?
        .with_timezone(&Utc);

    let raw_scope = take_string(&mut fields, "scope")?;
    let project_id = take_string(&mut fields, "project_id")?;
    let scope = match raw_scope.as_deref() {
        None => None,
        Some("global") => Some(KnowledgeScope::Global),
        Some("user") => Some(KnowledgeScope::User),
        Some("project") => Some(KnowledgeScope::Project {
            project_id: project_id.context("scope `project` requires `project_id`")?,
        }),
        Some(other) => bail!("unknown scope `{other}`"),
    };

    let agent_id = take_string(&mut fields, "agent_id")?;
    let event_id = take_string(&mut fields, "event_id")?;
    let tags = match fields.shift_remove("tags") {
        None => Vec::new(),
        Some(Value::Sequence(items)) => items
            .into_iter()
            .map(|item| match item {
                Value::String(tag) => Ok(tag),
                other => bail!("tag `{other:?}` is not a string"),
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
        Some(other) => bail!("`tags` must be a sequence of strings, got {other:?}"),
    };

    let file_name = relative_path
        .rsplit('/')
        .next()
        .unwrap_or(relative_path)
        .to_owned();
    let id = file_name
        .strip_suffix(".md")
        .unwrap_or(&file_name)
        .to_owned();

    Ok(KnowledgeConcept {
        id,
        relative_path: relative_path.to_owned(),
        concept_type,
        title,
        description,
        timestamp,
        body,
        scope,
        agent_id,
        event_id,
        tags,
        extensions: fields,
    })
}

/// Renders a concept back to OKF Markdown. Recognized fields come first in
/// canonical order, preserved unknown fields follow in their original order,
/// and the body is separated by one blank line and ends with one newline.
pub fn render_concept(concept: &KnowledgeConcept) -> anyhow::Result<String> {
    let mut fields = serde_yaml::Mapping::new();
    let mut put = |key: &str, value: Value| {
        fields.insert(Value::String(key.to_owned()), value);
    };
    put("type", Value::String(concept.concept_type.clone()));
    put("title", Value::String(concept.title.clone()));
    put("description", Value::String(concept.description.clone()));
    put(
        "timestamp",
        Value::String(concept.timestamp.to_rfc3339_opts(SecondsFormat::Secs, true)),
    );
    match &concept.scope {
        None => {}
        Some(KnowledgeScope::Global) => put("scope", Value::String("global".into())),
        Some(KnowledgeScope::User) => put("scope", Value::String("user".into())),
        Some(KnowledgeScope::Project { project_id }) => {
            put("scope", Value::String("project".into()));
            put("project_id", Value::String(project_id.clone()));
        }
    }
    if let Some(agent_id) = &concept.agent_id {
        put("agent_id", Value::String(agent_id.clone()));
    }
    if let Some(event_id) = &concept.event_id {
        put("event_id", Value::String(event_id.clone()));
    }
    if !concept.tags.is_empty() {
        put(
            "tags",
            Value::Sequence(concept.tags.iter().cloned().map(Value::String).collect()),
        );
    }
    for (key, value) in &concept.extensions {
        fields.insert(Value::String(key.clone()), value.clone());
    }
    let frontmatter =
        serde_yaml::to_string(&Value::Mapping(fields)).context("failed to render frontmatter")?;
    let body = concept.body.trim_matches(['\n', '\r']);
    if body.is_empty() {
        Ok(format!("---\n{frontmatter}---\n"))
    } else {
        Ok(format!("---\n{frontmatter}---\n\n{body}\n"))
    }
}

/// Validates a bundle-relative concept path: forward slashes only, no
/// traversal or absolute segments, the directory must be a known concept
/// area (memory scopes or learning areas), and the file name must be a
/// single safe `<id>.md` component that is not a reserved generated name.
pub fn validate_concept_relative_path(relative_path: &str) -> anyhow::Result<()> {
    if relative_path.contains('\\') {
        bail!("path `{relative_path}` must use forward slashes");
    }
    let (directory, file_name) = relative_path
        .rsplit_once('/')
        .with_context(|| format!("path `{relative_path}` is missing a concept directory"))?;
    validate_concept_directory(directory)
        .with_context(|| format!("path `{relative_path}` is not inside a concept area"))?;
    validate_path_component(file_name)
        .with_context(|| format!("path `{relative_path}` has an unsafe file name"))?;
    let stem = file_name
        .strip_suffix(".md")
        .with_context(|| format!("path `{relative_path}` must end with `.md`"))?;
    if stem.is_empty() {
        bail!("path `{relative_path}` has an empty concept id");
    }
    if RESERVED_FILE_NAMES.contains(&file_name) {
        bail!("`{file_name}` is a reserved generated file, not a concept");
    }
    Ok(())
}

/// All fixed concept directories (everything except per-project memory).
pub const FIXED_CONCEPT_DIRECTORIES: [&str; 7] = [
    "memory/global",
    "memory/user",
    "learning/skills",
    "learning/reviews",
    "learning/journey",
    "learning/curator",
    "learning/curator-history",
];

/// The parent directory of all per-project memory directories.
pub const PROJECT_MEMORY_PARENT: &str = "memory/projects";

fn validate_concept_directory(directory: &str) -> anyhow::Result<()> {
    if FIXED_CONCEPT_DIRECTORIES.contains(&directory) {
        return Ok(());
    }
    if let Some(project_id) = directory.strip_prefix("memory/projects/") {
        validate_path_component(project_id).context("invalid project id")?;
        return Ok(());
    }
    bail!("`{directory}` is not a concept directory");
}

/// A single safe path component: non-empty, no separators, no traversal,
/// and restricted to ASCII alphanumerics plus `.`, `_`, and `-`.
pub fn validate_path_component(value: &str) -> anyhow::Result<()> {
    if value.is_empty() || value.len() > 128 {
        bail!("component must be 1..=128 characters");
    }
    if value == "." || value == ".." {
        bail!("component must not be a traversal segment");
    }
    if !value
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
    {
        bail!("component `{value}` contains unsafe characters");
    }
    Ok(())
}

fn split_frontmatter(raw: &str) -> anyhow::Result<(String, String)> {
    let mut lines = raw.split_inclusive('\n');
    let first = lines.next().unwrap_or_default();
    if first.trim_end_matches(['\r', '\n']) != "---" {
        bail!("document must start with a `---` frontmatter line");
    }
    let mut frontmatter = String::new();
    for line in lines.by_ref() {
        if line.trim_end_matches(['\r', '\n']) == "---" {
            let body: String = lines.collect();
            let body = body.trim_matches(['\n', '\r']).to_owned();
            return Ok((frontmatter, body));
        }
        frontmatter.push_str(line);
    }
    bail!("frontmatter is missing its closing `---` line");
}

fn take_string(fields: &mut IndexMap<String, Value>, key: &str) -> anyhow::Result<Option<String>> {
    match fields.shift_remove(key) {
        None => Ok(None),
        Some(Value::String(value)) => Ok(Some(value)),
        Some(other) => bail!("`{key}` must be a string, got {other:?}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn okf_roundtrip_preserves_unknown_metadata_and_markdown_links() {
        let raw = "---\ntype: Memory\ntitle: Concise summaries\ndescription: Prefer concise summaries.\ntimestamp: 2026-07-12T14:30:00Z\nscope: user\nagent_id: ryuzi\nx_score: 0.9\n---\n\nUse concise summaries.\n\n[Session](ryuzi://sessions/c-1#turn-2)\n";
        let concept = parse_concept("memory/user/fact.md", raw).unwrap();
        let rendered = render_concept(&concept).unwrap();
        assert!(rendered.contains("x_score: 0.9"));
        assert!(rendered.contains("[Session](ryuzi://sessions/c-1#turn-2)"));
    }

    #[test]
    fn okf_requires_frontmatter_and_rfc3339_timestamp() {
        assert!(parse_concept("learning/reviews/a.md", "plain markdown").is_err());
        assert!(parse_concept(
            "learning/reviews/a.md",
            "---\ntype: Review\ntitle: A\ndescription: A\ntimestamp: yesterday\n---\nA"
        )
        .is_err());
    }

    #[test]
    fn relative_path_rejects_reserved_traversal_absolute_and_separator_ids() {
        for value in [
            "../outside.md",
            "/tmp/outside.md",
            "memory/index.md",
            "learning/../x.md",
            "memory/user/a/b.md",
        ] {
            assert!(
                validate_concept_relative_path(value).is_err(),
                "accepted {value}"
            );
        }
    }
}
