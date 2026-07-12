//! Per-agent OKF knowledge store: CRUD, batch, search, and generated
//! index/log files over the `agents/<id>/knowledge` bundle, with canonical
//! path containment, invalid-document tolerance, and per-agent write
//! serialization.

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use anyhow::{anyhow, bail, Context};
use chrono::{DateTime, SecondsFormat, Utc};

use crate::paths;

use super::okf::{
    parse_concept, render_concept, validate_concept_relative_path, validate_path_component,
    InvalidKnowledgeConcept, KnowledgeConcept, KnowledgeConceptInput, FIXED_CONCEPT_DIRECTORIES,
    PROJECT_MEMORY_PARENT, RESERVED_FILE_NAMES,
};
use super::registry::validate_agent_id;
use super::transaction::atomic_write;
use super::types::AgentId;

/// Directory the batch API stages complete bundle copies under. Never
/// scanned as concept content.
const TRANSACTIONS_DIR: &str = ".knowledge-transactions";

/// Everything the future Learning surface needs from one agent's bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentLearningSnapshot {
    pub concepts: Vec<KnowledgeConcept>,
    pub invalid: Vec<InvalidKnowledgeConcept>,
    pub journey: Vec<JourneyMilestone>,
    pub skill_usage: Vec<AgentSkillUsage>,
    pub reviews: Vec<LearningReview>,
    pub curator: CuratorState,
    pub curator_history: Vec<CuratorHistorySnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JourneyMilestone {
    pub concept_id: String,
    pub title: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSkillUsage {
    pub skill_id: String,
    pub uses: u64,
    pub successes: u64,
    pub concept_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LearningReview {
    pub concept_id: String,
    pub title: String,
    pub description: String,
    pub timestamp: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CuratorState {
    pub concept: Option<KnowledgeConcept>,
    pub last_event_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CuratorHistorySnapshot {
    pub snapshot_id: String,
    pub concept: KnowledgeConcept,
}

/// One mutation inside an all-or-nothing [`KnowledgeStore::batch`] call.
#[derive(Debug, Clone, PartialEq)]
pub enum KnowledgeOperation {
    Add(KnowledgeConceptInput),
    Replace {
        concept_id: String,
        input: KnowledgeConceptInput,
    },
    Remove {
        concept_id: String,
    },
}

/// The parse result of a full bundle walk: every valid concept plus every
/// Markdown file that failed OKF parsing, both sorted by relative path.
#[derive(Debug, Clone, PartialEq)]
pub struct KnowledgeScan {
    pub valid: Vec<KnowledgeConcept>,
    pub invalid: Vec<InvalidKnowledgeConcept>,
}

/// Factory handing out per-agent [`KnowledgeStore`]s that share one write
/// lock per agent id, so all writers created from the same factory
/// serialize their bundle mutations.
pub struct AgentKnowledgeStore {
    config_root: PathBuf,
    locks: Mutex<HashMap<AgentId, Arc<tokio::sync::Mutex<()>>>>,
}

impl AgentKnowledgeStore {
    pub fn new(config_root: PathBuf) -> Self {
        Self {
            config_root,
            locks: Mutex::new(HashMap::new()),
        }
    }

    pub fn for_agent(&self, agent_id: &str) -> anyhow::Result<KnowledgeStore> {
        validate_agent_id(agent_id).map_err(|issue| anyhow!(issue.message))?;
        let write_lock = self
            .locks
            .lock()
            .map_err(|_| anyhow!("knowledge lock registry is poisoned"))?
            .entry(agent_id.to_owned())
            .or_default()
            .clone();
        Ok(KnowledgeStore {
            agent_id: agent_id.to_owned(),
            root: paths::agent_knowledge_dir_in(&self.config_root, agent_id),
            write_lock,
        })
    }

    /// Reads one agent's bundle into the shape the Plan 3 Learning surface
    /// consumes: journey milestones, skill usage counters, reviews, the
    /// newest curator state, and curator history snapshots.
    pub async fn learning_snapshot(&self, agent_id: &str) -> anyhow::Result<AgentLearningSnapshot> {
        let scan = self.for_agent(agent_id)?.scan().await?;
        let mut journey = Vec::new();
        let mut skill_usage = Vec::new();
        let mut reviews = Vec::new();
        let mut curator_concepts = Vec::new();
        let mut curator_history = Vec::new();
        for concept in &scan.valid {
            let Some((directory, _)) = concept.relative_path.rsplit_once('/') else {
                continue;
            };
            match directory {
                "learning/journey" => journey.push(JourneyMilestone {
                    concept_id: concept.id.clone(),
                    title: concept.title.clone(),
                    timestamp: concept.timestamp,
                }),
                "learning/skills" => skill_usage.push(AgentSkillUsage {
                    skill_id: extension_string(concept, "skill_id")
                        .unwrap_or_else(|| concept.id.clone()),
                    uses: extension_u64(concept, "uses"),
                    successes: extension_u64(concept, "successes"),
                    concept_id: concept.id.clone(),
                }),
                "learning/reviews" => reviews.push(LearningReview {
                    concept_id: concept.id.clone(),
                    title: concept.title.clone(),
                    description: concept.description.clone(),
                    timestamp: concept.timestamp,
                }),
                "learning/curator" => curator_concepts.push(concept.clone()),
                "learning/curator-history" => curator_history.push(CuratorHistorySnapshot {
                    snapshot_id: concept.id.clone(),
                    concept: concept.clone(),
                }),
                _ => {}
            }
        }
        curator_concepts.sort_by_key(|concept| concept.timestamp);
        let curator_concept = curator_concepts.pop();
        let curator = CuratorState {
            last_event_id: curator_concept
                .as_ref()
                .and_then(|concept| concept.event_id.clone()),
            concept: curator_concept,
        };
        Ok(AgentLearningSnapshot {
            concepts: scan.valid,
            invalid: scan.invalid,
            journey,
            skill_usage,
            reviews,
            curator,
            curator_history,
        })
    }
}

fn extension_string(concept: &KnowledgeConcept, key: &str) -> Option<String> {
    match concept.extensions.get(key) {
        Some(serde_yaml::Value::String(value)) => Some(value.clone()),
        _ => None,
    }
}

fn extension_u64(concept: &KnowledgeConcept, key: &str) -> u64 {
    match concept.extensions.get(key) {
        Some(serde_yaml::Value::Number(value)) => value.as_u64().unwrap_or(0),
        _ => 0,
    }
}

/// One agent's knowledge bundle. All writes hold the shared per-agent lock
/// for the full write/index/log cycle and refuse any canonical path that
/// escapes the canonical bundle root.
pub struct KnowledgeStore {
    agent_id: AgentId,
    root: PathBuf,
    write_lock: Arc<tokio::sync::Mutex<()>>,
}

impl KnowledgeStore {
    pub fn root(&self) -> &Path {
        &self.root
    }

    pub async fn scan(&self) -> anyhow::Result<KnowledgeScan> {
        scan_bundle(&self.root)
    }

    pub async fn read(&self, concept_id: &str) -> anyhow::Result<KnowledgeConcept> {
        find_concept(&self.root, concept_id)?
            .ok_or_else(|| anyhow!("concept `{concept_id}` was not found"))
    }

    pub async fn create(&self, input: KnowledgeConceptInput) -> anyhow::Result<KnowledgeConcept> {
        let _guard = self.write_lock.lock().await;
        ensure_bundle_at(&self.root)?;
        let concept = self.concept_from_input(paths::new_id(), &input)?;
        self.write_concept(&concept)?;
        self.finish_write("create", &concept.id, &concept.relative_path)?;
        Ok(concept)
    }

    pub async fn update(
        &self,
        concept_id: &str,
        input: KnowledgeConceptInput,
    ) -> anyhow::Result<KnowledgeConcept> {
        let _guard = self.write_lock.lock().await;
        ensure_bundle_at(&self.root)?;
        let existing = find_concept(&self.root, concept_id)?
            .ok_or_else(|| anyhow!("concept `{concept_id}` was not found"))?;
        let concept = self.concept_from_input(concept_id.to_owned(), &input)?;
        self.write_concept(&concept)?;
        if existing.relative_path != concept.relative_path {
            fs::remove_file(self.safe_existing_path(&existing.relative_path)?)?;
        }
        self.finish_write("update", &concept.id, &concept.relative_path)?;
        Ok(concept)
    }

    pub async fn delete(&self, concept_id: &str) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        ensure_bundle_at(&self.root)?;
        let existing = find_concept(&self.root, concept_id)?
            .ok_or_else(|| anyhow!("concept `{concept_id}` was not found"))?;
        fs::remove_file(self.safe_existing_path(&existing.relative_path)?)?;
        self.finish_write("delete", concept_id, &existing.relative_path)?;
        Ok(())
    }

    /// Case-insensitive substring match over title, description, body, and
    /// tags of every valid concept.
    pub async fn search(&self, query: &str) -> anyhow::Result<Vec<KnowledgeConcept>> {
        let needle = query.to_lowercase();
        Ok(scan_bundle(&self.root)?
            .valid
            .into_iter()
            .filter(|concept| {
                concept.title.to_lowercase().contains(&needle)
                    || concept.description.to_lowercase().contains(&needle)
                    || concept.body.to_lowercase().contains(&needle)
                    || concept
                        .tags
                        .iter()
                        .any(|tag| tag.to_lowercase().contains(&needle))
            })
            .collect())
    }

    /// Memory concepts only. `None` returns every memory scope; a project id
    /// returns global + user + that project's memories (the set that applies
    /// when working inside the project).
    pub async fn list_memory(&self, project_id: Option<&str>) -> anyhow::Result<KnowledgeScan> {
        if let Some(project_id) = project_id {
            validate_path_component(project_id).context("invalid project id")?;
        }
        let scan = scan_bundle(&self.root)?;
        let keep = |relative_path: &str| {
            let Some((directory, _)) = relative_path.rsplit_once('/') else {
                return false;
            };
            match project_id {
                None => directory.starts_with("memory/"),
                Some(project_id) => {
                    directory == "memory/global"
                        || directory == "memory/user"
                        || directory == format!("{PROJECT_MEMORY_PARENT}/{project_id}")
                }
            }
        };
        Ok(KnowledgeScan {
            valid: scan
                .valid
                .into_iter()
                .filter(|concept| keep(&concept.relative_path))
                .collect(),
            invalid: scan
                .invalid
                .into_iter()
                .filter(|invalid| keep(&invalid.relative_path))
                .collect(),
        })
    }

    /// Parses raw Markdown as it would live at `relative_path` without
    /// touching the filesystem.
    pub async fn validate_raw(
        &self,
        relative_path: &str,
        raw: &str,
    ) -> anyhow::Result<KnowledgeConcept> {
        validate_concept_relative_path(relative_path)?;
        parse_concept(relative_path, raw)
    }

    /// Replaces (or repairs) the document at `relative_path` with raw
    /// Markdown that must parse as a valid concept.
    pub async fn replace_raw(
        &self,
        relative_path: &str,
        raw: &str,
    ) -> anyhow::Result<KnowledgeConcept> {
        validate_concept_relative_path(relative_path)?;
        let concept = parse_concept(relative_path, raw)?;
        let _guard = self.write_lock.lock().await;
        ensure_bundle_at(&self.root)?;
        let target = self.safe_target(relative_path)?;
        atomic_write(&target, render_concept(&concept)?.as_bytes())?;
        self.finish_write("replace", &concept.id, relative_path)?;
        Ok(concept)
    }

    /// Deletes a document that failed parsing. The path must still be a
    /// well-formed concept path so traversal can never delete other files.
    pub async fn delete_invalid(&self, relative_path: &str) -> anyhow::Result<()> {
        validate_concept_relative_path(relative_path)?;
        let _guard = self.write_lock.lock().await;
        ensure_bundle_at(&self.root)?;
        let target = self.safe_existing_path(relative_path)?;
        fs::remove_file(target)?;
        let stem = concept_id_of(relative_path);
        self.finish_write("delete-invalid", &stem, relative_path)?;
        Ok(())
    }

    /// Rebuilds `memory/index.md` and `learning/index.md` from the current
    /// valid concepts.
    pub async fn regenerate_indexes(&self) -> anyhow::Result<()> {
        let _guard = self.write_lock.lock().await;
        ensure_bundle_at(&self.root)?;
        self.regenerate_indexes_locked()
    }

    /// Applies every operation or none: the bundle is copied into a staging
    /// directory, all operations are applied and every resulting document is
    /// reparsed there, and only then are the affected files swapped into the
    /// active bundle (still under the per-agent lock). Any failure removes
    /// the staging copy and leaves the active bundle untouched.
    pub async fn batch(
        &self,
        operations: Vec<KnowledgeOperation>,
    ) -> anyhow::Result<Vec<KnowledgeConcept>> {
        let _guard = self.write_lock.lock().await;
        ensure_bundle_at(&self.root)?;
        let staging = self
            .root
            .join(TRANSACTIONS_DIR)
            .join(paths::new_id())
            .join("stage");
        let outcome = self.stage_batch(&staging, &operations);
        let staged = match outcome {
            Ok(staged) => staged,
            Err(error) => {
                let _ = fs::remove_dir_all(staging.parent().unwrap_or(&staging));
                return Err(error);
            }
        };
        // Every operation validated against the complete staged copy; swap
        // the affected files into the active bundle.
        for (relative_path, content) in &staged.writes {
            let target = self.safe_target(relative_path)?;
            atomic_write(&target, content.as_bytes())?;
        }
        for relative_path in &staged.removes {
            let target = self.safe_existing_path(relative_path)?;
            fs::remove_file(target)?;
        }
        self.regenerate_indexes_locked()?;
        for (action, concept_id, relative_path) in &staged.log_lines {
            self.append_log(action, concept_id, relative_path)?;
        }
        let _ = fs::remove_dir_all(staging.parent().unwrap_or(&staging));
        Ok(staged.results)
    }

    fn stage_batch(
        &self,
        staging: &Path,
        operations: &[KnowledgeOperation],
    ) -> anyhow::Result<StagedBatch> {
        copy_concept_files(&self.root, staging)?;
        let mut staged = StagedBatch::default();
        for operation in operations {
            match operation {
                KnowledgeOperation::Add(input) => {
                    let concept = self.concept_from_input(paths::new_id(), input)?;
                    stage_concept(staging, &concept)?;
                    staged.record_write(&concept, "add");
                    staged.results.push(concept);
                }
                KnowledgeOperation::Replace { concept_id, input } => {
                    let existing = find_concept(staging, concept_id)?
                        .ok_or_else(|| anyhow!("concept `{concept_id}` was not found"))?;
                    let concept = self.concept_from_input(concept_id.clone(), input)?;
                    stage_concept(staging, &concept)?;
                    if existing.relative_path != concept.relative_path {
                        fs::remove_file(staging.join(&existing.relative_path))?;
                        staged.removes.push(existing.relative_path);
                    }
                    staged.record_write(&concept, "replace");
                    staged.results.push(concept);
                }
                KnowledgeOperation::Remove { concept_id } => {
                    let existing = find_concept(staging, concept_id)?
                        .ok_or_else(|| anyhow!("concept `{concept_id}` was not found"))?;
                    fs::remove_file(staging.join(&existing.relative_path))?;
                    staged.log_lines.push((
                        "remove".into(),
                        concept_id.clone(),
                        existing.relative_path.clone(),
                    ));
                    staged.removes.push(existing.relative_path);
                }
            }
        }
        // Reparse every document the batch produced before exposing anything.
        for (relative_path, content) in &staged.writes {
            parse_concept(relative_path, content)
                .with_context(|| format!("staged `{relative_path}` failed to reparse"))?;
        }
        Ok(staged)
    }

    fn concept_from_input(
        &self,
        id: String,
        input: &KnowledgeConceptInput,
    ) -> anyhow::Result<KnowledgeConcept> {
        if input.title.trim().is_empty() {
            bail!("concept title must not be blank");
        }
        if input.description.trim().is_empty() {
            bail!("concept description must not be blank");
        }
        let directory = input.area.directory()?;
        let relative_path = format!("{directory}/{id}.md");
        validate_concept_relative_path(&relative_path)?;
        let timestamp = DateTime::from_timestamp(Utc::now().timestamp(), 0)
            .context("system clock is out of range")?;
        Ok(KnowledgeConcept {
            id,
            relative_path,
            concept_type: input.area.concept_type().to_owned(),
            title: input.title.clone(),
            description: input.description.clone(),
            timestamp,
            body: input.body.clone(),
            scope: input.area.scope(),
            agent_id: Some(self.agent_id.clone()),
            event_id: None,
            tags: input.tags.clone(),
            extensions: input.extensions.clone(),
        })
    }

    fn write_concept(&self, concept: &KnowledgeConcept) -> anyhow::Result<()> {
        let target = self.safe_target(&concept.relative_path)?;
        atomic_write(&target, render_concept(concept)?.as_bytes())
    }

    /// Regenerates indexes and appends one log line; every mutation ends
    /// its locked cycle here.
    fn finish_write(
        &self,
        action: &str,
        concept_id: &str,
        relative_path: &str,
    ) -> anyhow::Result<()> {
        self.regenerate_indexes_locked()?;
        self.append_log(action, concept_id, relative_path)
    }

    fn regenerate_indexes_locked(&self) -> anyhow::Result<()> {
        let scan = scan_bundle(&self.root)?;
        for area in ["memory", "learning"] {
            let prefix = format!("{area}/");
            let mut lines: Vec<String> = scan
                .valid
                .iter()
                .filter_map(|concept| {
                    concept.relative_path.strip_prefix(&prefix).map(|link| {
                        format!("- [{}]({link}) — {}", concept.title, concept.description)
                    })
                })
                .collect();
            lines.sort();
            let mut content = lines.join("\n");
            if !content.is_empty() {
                content.push('\n');
            }
            atomic_write(&self.root.join(area).join("index.md"), content.as_bytes())?;
        }
        Ok(())
    }

    /// Appends one line to the root `log.md`: timestamp, action, concept id,
    /// and relative path — never document bodies.
    fn append_log(
        &self,
        action: &str,
        concept_id: &str,
        relative_path: &str,
    ) -> anyhow::Result<()> {
        let log_path = self.root.join("log.md");
        let mut content = match fs::read_to_string(&log_path) {
            Ok(existing) => existing,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        let timestamp = Utc::now().to_rfc3339_opts(SecondsFormat::Secs, true);
        content.push_str(&format!(
            "- {timestamp} {action} {concept_id} {relative_path}\n"
        ));
        atomic_write(&log_path, content.as_bytes())
    }

    /// Resolves a concept path for writing: creates the parent, then rejects
    /// it when its canonical form (symlinks resolved) leaves the canonical
    /// bundle root.
    fn safe_target(&self, relative_path: &str) -> anyhow::Result<PathBuf> {
        let full = self.root.join(relative_path);
        let parent = full
            .parent()
            .context("concept path has no parent directory")?;
        fs::create_dir_all(parent)?;
        let file_name = full.file_name().context("concept path has no file name")?;
        Ok(self.checked_canonical(parent)?.join(file_name))
    }

    /// Resolves an existing concept path for reads/removal with the same
    /// canonical containment check as [`Self::safe_target`].
    fn safe_existing_path(&self, relative_path: &str) -> anyhow::Result<PathBuf> {
        let full = self.root.join(relative_path);
        let parent = full
            .parent()
            .context("concept path has no parent directory")?;
        let file_name = full.file_name().context("concept path has no file name")?;
        let target = self.checked_canonical(parent)?.join(file_name);
        if !target.is_file() {
            bail!("`{relative_path}` does not exist");
        }
        Ok(target)
    }

    fn checked_canonical(&self, parent: &Path) -> anyhow::Result<PathBuf> {
        let canonical_root = fs::canonicalize(&self.root)
            .with_context(|| format!("bundle root {} is missing", self.root.display()))?;
        let canonical_parent = fs::canonicalize(parent)
            .with_context(|| format!("failed to canonicalize {}", parent.display()))?;
        if !canonical_parent.starts_with(&canonical_root) {
            bail!(
                "path {} escapes the knowledge bundle",
                canonical_parent.display()
            );
        }
        Ok(canonical_parent)
    }
}

#[derive(Default)]
struct StagedBatch {
    writes: Vec<(String, String)>,
    removes: Vec<String>,
    log_lines: Vec<(String, String, String)>,
    results: Vec<KnowledgeConcept>,
}

impl StagedBatch {
    fn record_write(&mut self, concept: &KnowledgeConcept, action: &str) {
        // A later operation may rewrite the same path; last write wins.
        if let Ok(rendered) = render_concept(concept) {
            self.writes
                .retain(|(path, _)| path != &concept.relative_path);
            self.writes.push((concept.relative_path.clone(), rendered));
        }
        self.log_lines.push((
            action.to_owned(),
            concept.id.clone(),
            concept.relative_path.clone(),
        ));
    }
}

fn stage_concept(staging: &Path, concept: &KnowledgeConcept) -> anyhow::Result<()> {
    let target = staging.join(&concept.relative_path);
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    atomic_write(&target, render_concept(concept)?.as_bytes())
}

/// Creates the full bundle skeleton: every fixed concept directory, the
/// project-memory parent, and empty generated `index.md`/`log.md` files.
/// Idempotent; never overwrites existing generated files.
pub fn ensure_bundle_at(root: &Path) -> anyhow::Result<()> {
    for directory in FIXED_CONCEPT_DIRECTORIES {
        fs::create_dir_all(root.join(directory))?;
    }
    fs::create_dir_all(root.join(PROJECT_MEMORY_PARENT))?;
    for generated in ["memory/index.md", "learning/index.md", "log.md"] {
        let path = root.join(generated);
        if !path.exists() {
            atomic_write(&path, b"")?;
        }
    }
    Ok(())
}

/// Every directory that may contain concept documents right now: the fixed
/// areas plus each existing per-project memory directory.
fn concept_directories(root: &Path) -> anyhow::Result<Vec<String>> {
    let mut directories: Vec<String> = FIXED_CONCEPT_DIRECTORIES
        .iter()
        .map(|directory| (*directory).to_owned())
        .collect();
    let projects = root.join(PROJECT_MEMORY_PARENT);
    if projects.is_dir() {
        let mut project_ids: Vec<String> = fs::read_dir(&projects)?
            .filter_map(Result::ok)
            .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
            .filter_map(|entry| entry.file_name().to_str().map(str::to_owned))
            .collect();
        project_ids.sort();
        for project_id in project_ids {
            directories.push(format!("{PROJECT_MEMORY_PARENT}/{project_id}"));
        }
    }
    Ok(directories)
}

fn scan_bundle(root: &Path) -> anyhow::Result<KnowledgeScan> {
    let mut valid = Vec::new();
    let mut invalid = Vec::new();
    for directory in concept_directories(root)? {
        let path = root.join(&directory);
        if !path.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&path)?.filter_map(Result::ok) {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !name.ends_with(".md") || RESERVED_FILE_NAMES.contains(&name.as_str()) {
                continue;
            }
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            let relative_path = format!("{directory}/{name}");
            let raw = fs::read_to_string(entry.path())?;
            match parse_concept(&relative_path, &raw) {
                Ok(concept) => valid.push(concept),
                Err(error) => invalid.push(InvalidKnowledgeConcept {
                    relative_path,
                    error: format!("{error:#}"),
                    raw_markdown: raw,
                }),
            }
        }
    }
    valid.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    invalid.sort_by(|a, b| a.relative_path.cmp(&b.relative_path));
    Ok(KnowledgeScan { valid, invalid })
}

fn find_concept(root: &Path, concept_id: &str) -> anyhow::Result<Option<KnowledgeConcept>> {
    validate_path_component(concept_id).context("invalid concept id")?;
    Ok(scan_bundle(root)?
        .valid
        .into_iter()
        .find(|concept| concept.id == concept_id))
}

fn concept_id_of(relative_path: &str) -> String {
    let file_name = relative_path.rsplit('/').next().unwrap_or(relative_path);
    file_name
        .strip_suffix(".md")
        .unwrap_or(file_name)
        .to_owned()
}

/// Copies every current concept document (valid or invalid) into the
/// staging root, preserving relative paths, so batch operations validate
/// against a complete bundle copy.
fn copy_concept_files(root: &Path, staging: &Path) -> anyhow::Result<()> {
    for directory in concept_directories(root)? {
        let source = root.join(&directory);
        let destination = staging.join(&directory);
        fs::create_dir_all(&destination)?;
        if !source.is_dir() {
            continue;
        }
        for entry in fs::read_dir(&source)?.filter_map(Result::ok) {
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue;
            };
            if !name.ends_with(".md") || RESERVED_FILE_NAMES.contains(&name.as_str()) {
                continue;
            }
            if !entry.file_type().is_ok_and(|kind| kind.is_file()) {
                continue;
            }
            fs::copy(entry.path(), destination.join(&name))?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use indexmap::IndexMap;
    use serde_yaml::Value;

    use super::super::okf::{ConceptArea, KnowledgeConceptInput, KnowledgeScope};
    use super::*;

    fn concept_input(area: ConceptArea, title: &str) -> KnowledgeConceptInput {
        KnowledgeConceptInput {
            area,
            title: title.into(),
            description: format!("{title} description"),
            body: format!("{title} body"),
            tags: Vec::new(),
            extensions: IndexMap::new(),
        }
    }

    fn memory_input(scope: KnowledgeScope, title: &str) -> KnowledgeConceptInput {
        concept_input(ConceptArea::Memory(scope), title)
    }

    fn fixture_store(agent_id: &str) -> (tempfile::TempDir, KnowledgeStore) {
        let root = tempfile::tempdir().unwrap();
        let store = AgentKnowledgeStore::new(root.path().to_path_buf())
            .for_agent(agent_id)
            .unwrap();
        (root, store)
    }

    #[tokio::test]
    async fn concepts_are_agent_and_project_isolated_and_invalid_files_remain_visible() {
        let root = tempfile::tempdir().unwrap();
        let store = AgentKnowledgeStore::new(root.path().to_path_buf());
        let a = store.for_agent("a").unwrap();
        let b = store.for_agent("b").unwrap();
        a.create(memory_input(
            KnowledgeScope::Project {
                project_id: "p1".into(),
            },
            "A fact",
        ))
        .await
        .unwrap();
        b.create(memory_input(
            KnowledgeScope::Project {
                project_id: "p1".into(),
            },
            "B fact",
        ))
        .await
        .unwrap();
        std::fs::write(
            a.root().join("learning/reviews/broken.md"),
            "not frontmatter",
        )
        .unwrap();
        assert_eq!(a.list_memory(Some("p1")).await.unwrap().valid.len(), 1);
        assert_eq!(b.list_memory(Some("p1")).await.unwrap().valid.len(), 1);
        assert_eq!(
            a.scan().await.unwrap().invalid[0].relative_path,
            "learning/reviews/broken.md"
        );
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn escaping_symlink_is_rejected() {
        use std::os::unix::fs::symlink;
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let store = AgentKnowledgeStore::new(root.path().to_path_buf())
            .for_agent("a")
            .unwrap();
        std::fs::create_dir_all(store.root().join("memory")).unwrap();
        symlink(outside.path(), store.root().join("memory/user")).unwrap();
        assert!(store
            .create(memory_input(KnowledgeScope::User, "escape"))
            .await
            .is_err());
        assert!(std::fs::read_dir(outside.path()).unwrap().next().is_none());
    }

    #[tokio::test]
    async fn batch_validates_every_operation_before_exposing_changes() {
        let (_root, store) = fixture_store("a");
        let first = store
            .create(memory_input(KnowledgeScope::User, "first"))
            .await
            .unwrap();
        let before = store.scan().await.unwrap();
        let result = store
            .batch(vec![
                KnowledgeOperation::Replace {
                    concept_id: first.id.clone(),
                    input: memory_input(KnowledgeScope::User, "changed"),
                },
                KnowledgeOperation::Remove {
                    concept_id: "missing".into(),
                },
            ])
            .await;
        assert!(result.is_err());
        assert_eq!(store.scan().await.unwrap().valid, before.valid);
    }

    #[tokio::test]
    async fn concurrent_writes_keep_both_concepts_and_well_formed_index() {
        let (_root, store) = fixture_store("a");
        let store = Arc::new(store);
        let (a, b) = tokio::join!(
            store.create(memory_input(KnowledgeScope::Global, "one")),
            store.create(memory_input(KnowledgeScope::Global, "two"))
        );
        assert!(a.is_ok() && b.is_ok());
        assert_eq!(store.list_memory(None).await.unwrap().valid.len(), 2);
        assert_eq!(
            std::fs::read_to_string(store.root().join("memory/index.md"))
                .unwrap()
                .matches(".md)")
                .count(),
            2
        );
    }

    #[tokio::test]
    async fn batch_applies_all_operations_and_returns_resulting_concepts() {
        let (_root, store) = fixture_store("a");
        let first = store
            .create(memory_input(KnowledgeScope::User, "first"))
            .await
            .unwrap();
        let results = store
            .batch(vec![
                KnowledgeOperation::Add(memory_input(KnowledgeScope::Global, "added")),
                KnowledgeOperation::Replace {
                    concept_id: first.id.clone(),
                    input: memory_input(KnowledgeScope::User, "changed"),
                },
            ])
            .await
            .unwrap();
        assert_eq!(results.len(), 2);
        assert_eq!(results[0].title, "added");
        assert_eq!(results[1].title, "changed");
        let scan = store.scan().await.unwrap();
        assert_eq!(scan.valid.len(), 2);
        assert!(scan.valid.iter().any(|c| c.title == "changed"));
        let remove = store
            .batch(vec![KnowledgeOperation::Remove {
                concept_id: first.id.clone(),
            }])
            .await
            .unwrap();
        assert!(remove.is_empty());
        assert_eq!(store.scan().await.unwrap().valid.len(), 1);
    }

    #[tokio::test]
    async fn crud_search_reserved_names_and_generated_files() {
        let (_root, store) = fixture_store("a");
        let created = store
            .create(memory_input(KnowledgeScope::Global, "Alpha fact"))
            .await
            .unwrap();
        assert_eq!(store.read(&created.id).await.unwrap().title, "Alpha fact");
        let updated = store
            .update(&created.id, memory_input(KnowledgeScope::User, "Beta fact"))
            .await
            .unwrap();
        assert_eq!(
            updated.relative_path,
            format!("memory/user/{}.md", created.id)
        );
        assert!(!store.root().join(&created.relative_path).exists());
        assert_eq!(store.search("beta").await.unwrap().len(), 1);
        assert!(store.search("alpha").await.unwrap().is_empty());
        // Reserved generated names are never writable as concepts.
        let raw = "---\ntype: Memory\ntitle: X\ndescription: X\ntimestamp: 2026-07-12T14:30:00Z\n---\nX\n";
        assert!(store.replace_raw("memory/index.md", raw).await.is_err());
        assert!(store.replace_raw("learning/log.md", raw).await.is_err());
        let index = std::fs::read_to_string(store.root().join("memory/index.md")).unwrap();
        assert!(index.contains(&format!(
            "- [Beta fact](user/{}.md) — Beta fact description",
            created.id
        )));
        let log = std::fs::read_to_string(store.root().join("log.md")).unwrap();
        assert!(log.contains("create") && log.contains(&created.id));
        assert!(!log.contains("Alpha fact body"));
        store.delete(&created.id).await.unwrap();
        assert!(store.read(&created.id).await.is_err());
        assert!(
            std::fs::read_to_string(store.root().join("memory/index.md"))
                .unwrap()
                .trim()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn invalid_documents_can_be_validated_repaired_or_deleted() {
        let (_root, store) = fixture_store("a");
        store
            .create(memory_input(KnowledgeScope::User, "seed"))
            .await
            .unwrap();
        std::fs::write(store.root().join("learning/reviews/broken.md"), "nope").unwrap();
        assert_eq!(store.scan().await.unwrap().invalid.len(), 1);
        let raw = "---\ntype: Review\ntitle: Fixed\ndescription: Fixed review.\ntimestamp: 2026-07-12T14:30:00Z\n---\n\nFixed body.\n";
        assert!(store
            .validate_raw("learning/reviews/broken.md", "still broken")
            .await
            .is_err());
        let repaired = store
            .validate_raw("learning/reviews/broken.md", raw)
            .await
            .unwrap();
        assert_eq!(repaired.title, "Fixed");
        store
            .replace_raw("learning/reviews/broken.md", raw)
            .await
            .unwrap();
        assert!(store.scan().await.unwrap().invalid.is_empty());
        std::fs::write(store.root().join("learning/reviews/broken2.md"), "nope").unwrap();
        store
            .delete_invalid("learning/reviews/broken2.md")
            .await
            .unwrap();
        assert!(store.scan().await.unwrap().invalid.is_empty());
        assert!(store.delete_invalid("../outside.md").await.is_err());
    }

    #[tokio::test]
    async fn learning_snapshot_maps_learning_areas() {
        let root = tempfile::tempdir().unwrap();
        let stores = AgentKnowledgeStore::new(root.path().to_path_buf());
        let store = stores.for_agent("a").unwrap();
        store
            .create(concept_input(ConceptArea::Journey, "Milestone"))
            .await
            .unwrap();
        store
            .create(concept_input(ConceptArea::Review, "Review one"))
            .await
            .unwrap();
        let mut skill = concept_input(ConceptArea::Skill, "Skill use");
        skill
            .extensions
            .insert("skill_id".into(), Value::String("commit".into()));
        skill
            .extensions
            .insert("uses".into(), Value::Number(3.into()));
        skill
            .extensions
            .insert("successes".into(), Value::Number(2.into()));
        store.create(skill).await.unwrap();
        let curator = store
            .create(concept_input(ConceptArea::CuratorState, "Curator"))
            .await
            .unwrap();
        store
            .create(concept_input(ConceptArea::CuratorHistory, "Snapshot"))
            .await
            .unwrap();
        let snapshot = stores.learning_snapshot("a").await.unwrap();
        assert_eq!(snapshot.journey.len(), 1);
        assert_eq!(snapshot.journey[0].title, "Milestone");
        assert_eq!(snapshot.reviews.len(), 1);
        assert_eq!(snapshot.skill_usage.len(), 1);
        assert_eq!(snapshot.skill_usage[0].skill_id, "commit");
        assert_eq!(snapshot.skill_usage[0].uses, 3);
        assert_eq!(snapshot.skill_usage[0].successes, 2);
        assert_eq!(snapshot.curator.concept.as_ref().unwrap().id, curator.id);
        assert_eq!(snapshot.curator_history.len(), 1);
        assert_eq!(snapshot.concepts.len(), 5);
        assert!(snapshot.invalid.is_empty());
    }
}
