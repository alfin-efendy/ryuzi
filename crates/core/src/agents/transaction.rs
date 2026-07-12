use std::fs::{self, File};
use std::io::Write;
use std::path::{Component, Path, PathBuf};

use anyhow::{anyhow, bail, Context};
use serde::{Deserialize, Serialize};

use crate::paths;

use super::types::{AgentRecoveryNotice, RegistryDiskImage};

const JOURNAL_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JournalPhase {
    Prepared,
    Committed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct TransactionJournal {
    schema_version: u32,
    transaction_id: String,
    phase: JournalPhase,
    operations: Vec<JournalOperation>,
    index_stage: String,
    index_target: String,
    index_backup: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum JournalOperation {
    Replace {
        stage: String,
        target: String,
        backup: String,
    },
    Create {
        stage: String,
        target: String,
    },
    Delete {
        target: String,
        trash: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransactionFailpoint {
    None,
    BeforeIndexReplace,
    AfterIndexReplaceBeforeCommitMarker,
}

pub struct AgentTransaction {
    config_root: PathBuf,
    dir: PathBuf,
    journal: TransactionJournal,
    _lock: fd_lock::RwLock<File>,
    failpoint: TransactionFailpoint,
}

impl AgentTransaction {
    pub fn prepare(config_root: &Path, candidate: &RegistryDiskImage) -> anyhow::Result<Self> {
        let agents_root = config_root.join("agents");
        fs::create_dir_all(&agents_root)?;
        let transactions_root = agents_root.join(".transactions");
        fs::create_dir_all(&transactions_root)?;

        let lock_path = agents_root.join(".registry.lock");
        let lock_file = File::options()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(&lock_path)?;
        let mut lock = fd_lock::RwLock::new(lock_file);
        let guard = lock
            .write()
            .map_err(|error| anyhow!("failed to lock agent registry: {error}"))?;
        std::mem::forget(guard);

        let transaction_id = paths::new_id();
        let relative_dir = format!("agents/.transactions/{transaction_id}");
        let dir = config_root.join(path_from_relative(&relative_dir)?);
        fs::create_dir_all(dir.join("stage/agents"))?;
        fs::create_dir_all(dir.join("backup/agents"))?;
        fs::create_dir_all(dir.join("trash/agents"))?;

        let mut operations = Vec::new();
        for (id, yaml) in &candidate.agents {
            validate_leaf_id(id)?;
            let target = format!("agents/{id}");
            let stage = format!("{relative_dir}/stage/agents/{id}");
            let backup = format!("{relative_dir}/backup/agents/{id}");
            let target_path = config_root.join(path_from_relative(&target)?);
            let stage_path = config_root.join(path_from_relative(&stage)?);
            if target_path.exists() {
                copy_directory(&target_path, &stage_path)?;
                operations.push(JournalOperation::Replace {
                    stage: stage.clone(),
                    target,
                    backup,
                });
            } else {
                fs::create_dir_all(&stage_path)?;
                operations.push(JournalOperation::Create {
                    stage: stage.clone(),
                    target,
                });
            }
            write_synced(&stage_path.join("agent.yaml"), yaml.as_bytes())?;
        }

        let subagents_target = "agents/subagents.yaml".to_owned();
        let subagents_stage = format!("{relative_dir}/stage/agents/subagents.yaml");
        let subagents_backup = format!("{relative_dir}/backup/agents/subagents.yaml");
        write_synced(
            &config_root.join(path_from_relative(&subagents_stage)?),
            candidate.subagents_yaml.as_bytes(),
        )?;
        if config_root.join("agents/subagents.yaml").exists() {
            operations.push(JournalOperation::Replace {
                stage: subagents_stage,
                target: subagents_target,
                backup: subagents_backup,
            });
        } else {
            operations.push(JournalOperation::Create {
                stage: subagents_stage,
                target: subagents_target,
            });
        }

        for id in &candidate.deleted_agent_ids {
            validate_leaf_id(id)?;
            operations.push(JournalOperation::Delete {
                target: format!("agents/{id}"),
                trash: format!("{relative_dir}/trash/agents/{id}"),
            });
        }

        let index_stage = format!("{relative_dir}/stage/agents/index.yaml");
        let index_target = "agents/index.yaml".to_owned();
        let index_backup = format!("{relative_dir}/backup/agents/index.yaml");
        write_synced(
            &config_root.join(path_from_relative(&index_stage)?),
            candidate.index_yaml.as_bytes(),
        )?;

        let journal = TransactionJournal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            transaction_id,
            phase: JournalPhase::Prepared,
            operations,
            index_stage,
            index_target,
            index_backup,
        };
        write_journal(&dir, &journal)?;
        sync_directory(&dir)?;
        sync_directory(&transactions_root)?;

        Ok(Self {
            config_root: config_root.to_owned(),
            dir,
            journal,
            _lock: lock,
            failpoint: TransactionFailpoint::None,
        })
    }

    #[cfg(test)]
    pub(crate) fn with_failpoint(mut self, failpoint: TransactionFailpoint) -> Self {
        self.failpoint = failpoint;
        self
    }

    pub fn commit(mut self) -> anyhow::Result<()> {
        let result = self.commit_before_marker();
        if let Err(commit_error) = result {
            return match rollback_prepared(&self.config_root, &self.journal) {
                Ok(()) => {
                    remove_transaction_dir(&self.dir)?;
                    Err(commit_error)
                }
                Err(rollback_error) => Err(anyhow!(
                    "transaction {} failed: {commit_error:#}; rollback failed: {rollback_error:#}",
                    self.journal.transaction_id
                )),
            };
        }

        self.journal.phase = JournalPhase::Committed;
        write_journal(&self.dir, &self.journal)?;
        sync_directory(&self.dir)?;
        if let Err(error) = cleanup_committed(&self.config_root, &self.dir, &self.journal) {
            tracing::warn!(
                transaction_id = %self.journal.transaction_id,
                error = %error,
                "agent transaction committed; cleanup deferred to recovery"
            );
        }
        Ok(())
    }

    fn commit_before_marker(&self) -> anyhow::Result<()> {
        for operation in &self.journal.operations {
            match operation {
                JournalOperation::Replace { target, backup, .. } => {
                    move_path(&self.config_root, target, backup)?;
                }
                JournalOperation::Delete { target, trash } => {
                    move_path(&self.config_root, target, trash)?;
                }
                JournalOperation::Create { .. } => {}
            }
        }
        for operation in &self.journal.operations {
            match operation {
                JournalOperation::Replace { stage, target, .. }
                | JournalOperation::Create { stage, target } => {
                    move_path(&self.config_root, stage, target)?;
                }
                JournalOperation::Delete { .. } => {}
            }
        }
        if self.failpoint == TransactionFailpoint::BeforeIndexReplace {
            bail!("injected failure before index replace");
        }
        let index_target = checked_join(&self.config_root, &self.journal.index_target)?;
        let index_backup = checked_join(&self.config_root, &self.journal.index_backup)?;
        if index_target.exists() {
            rename_path(&index_target, &index_backup)?;
        }
        let staged = fs::read(checked_join(&self.config_root, &self.journal.index_stage)?)?;
        atomic_write(&index_target, &staged)?;
        sync_directory(index_target.parent().context("index has no parent")?)?;
        if self.failpoint == TransactionFailpoint::AfterIndexReplaceBeforeCommitMarker {
            bail!("injected failure after index replace before commit marker");
        }
        Ok(())
    }
}

pub fn recover_transactions(config_root: &Path) -> anyhow::Result<Vec<AgentRecoveryNotice>> {
    let root = config_root.join("agents/.transactions");
    let entries = match fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    let mut dirs = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    dirs.sort();
    let mut notices = Vec::new();
    for dir in dirs {
        let journal: TransactionJournal =
            serde_json::from_slice(&fs::read(dir.join("journal.json")).with_context(|| {
                format!("transaction journal is unreadable in {}", dir.display())
            })?)?;
        validate_journal(&journal)?;
        let directory_id = dir
            .file_name()
            .and_then(|name| name.to_str())
            .context("transaction directory has an invalid name")?;
        if directory_id != journal.transaction_id {
            bail!("transaction journal id does not match its directory");
        }
        let code = match journal.phase {
            JournalPhase::Prepared => {
                rollback_prepared(config_root, &journal)?;
                "transaction-rolled-back"
            }
            JournalPhase::Committed => {
                replay_committed(config_root, &journal)?;
                "transaction-replayed"
            }
        };
        remove_transaction_dir(&dir)?;
        notices.push(AgentRecoveryNotice {
            code: code.into(),
            message: journal.transaction_id,
        });
    }
    Ok(notices)
}

pub fn atomic_write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    let parent = path.parent().context("atomic target has no parent")?;
    fs::create_dir_all(parent)?;
    let mut temporary = tempfile::NamedTempFile::new_in(parent)?;
    temporary.write_all(bytes)?;
    temporary.flush()?;
    temporary.as_file().sync_all()?;
    temporary
        .persist(path)
        .map_err(|error| error.error)
        .with_context(|| format!("failed to atomically replace {}", path.display()))?;
    sync_directory(parent)
}

fn rollback_prepared(config_root: &Path, journal: &TransactionJournal) -> anyhow::Result<()> {
    for operation in journal.operations.iter().rev() {
        match operation {
            JournalOperation::Replace { target, backup, .. } => {
                let target = checked_join(config_root, target)?;
                let backup = checked_join(config_root, backup)?;
                if backup.exists() {
                    if target.exists() {
                        remove_path(&target)?;
                    }
                    rename_path(&backup, &target)?;
                }
            }
            JournalOperation::Create { target, .. } => {
                let target = checked_join(config_root, target)?;
                if target.exists() {
                    remove_path(&target)?;
                    sync_parent(&target)?;
                }
            }
            JournalOperation::Delete { target, trash } => {
                let target = checked_join(config_root, target)?;
                let trash = checked_join(config_root, trash)?;
                match (target.exists(), trash.exists()) {
                    (false, true) => rename_path(&trash, &target)?,
                    (true, false) => {}
                    (true, true) => bail!("rollback has both delete target and trash"),
                    (false, false) => bail!("rollback has neither delete target nor trash"),
                }
            }
        }
    }
    let index_target = checked_join(config_root, &journal.index_target)?;
    let index_backup = checked_join(config_root, &journal.index_backup)?;
    if index_backup.exists() {
        if index_target.exists() {
            remove_path(&index_target)?;
        }
        rename_path(&index_backup, &index_target)?;
    }
    Ok(())
}

fn replay_committed(config_root: &Path, journal: &TransactionJournal) -> anyhow::Result<()> {
    for operation in &journal.operations {
        match operation {
            JournalOperation::Replace { stage, target, .. }
            | JournalOperation::Create { stage, target } => {
                let stage = checked_join(config_root, stage)?;
                let target = checked_join(config_root, target)?;
                match (stage.exists(), target.exists()) {
                    (true, false) => rename_path(&stage, &target)?,
                    (false, true) => {}
                    (true, true) => bail!("replay has both staged and active target"),
                    (false, false) => bail!("replay has neither staged nor active target"),
                }
            }
            JournalOperation::Delete { target, trash } => {
                let target = checked_join(config_root, target)?;
                let trash = checked_join(config_root, trash)?;
                match (target.exists(), trash.exists()) {
                    (false, true) | (false, false) => {}
                    (true, false) => bail!("committed delete target is still active"),
                    (true, true) => bail!("committed delete has both target and trash"),
                }
            }
        }
    }
    let index_stage = checked_join(config_root, &journal.index_stage)?;
    if index_stage.exists() {
        let staged = fs::read(index_stage)?;
        atomic_write(&checked_join(config_root, &journal.index_target)?, &staged)?;
    } else if !checked_join(config_root, &journal.index_target)?.exists() {
        bail!("replay has neither staged nor active index");
    }
    Ok(())
}

fn cleanup_committed(
    config_root: &Path,
    dir: &Path,
    journal: &TransactionJournal,
) -> anyhow::Result<()> {
    replay_committed(config_root, journal)?;
    remove_transaction_dir(dir)
}

fn remove_transaction_dir(dir: &Path) -> anyhow::Result<()> {
    if dir.exists() {
        fs::remove_dir_all(dir)?;
        if let Some(parent) = dir.parent() {
            sync_directory(parent)?;
        }
    }
    Ok(())
}

fn write_journal(dir: &Path, journal: &TransactionJournal) -> anyhow::Result<()> {
    let bytes = serde_json::to_vec_pretty(journal)?;
    atomic_write(&dir.join("journal.json"), &bytes)
}

fn validate_journal(journal: &TransactionJournal) -> anyhow::Result<()> {
    if journal.schema_version != JOURNAL_SCHEMA_VERSION {
        bail!("unsupported transaction journal schema");
    }
    for path in [
        &journal.index_stage,
        &journal.index_target,
        &journal.index_backup,
    ] {
        path_from_relative(path)?;
    }
    for operation in &journal.operations {
        match operation {
            JournalOperation::Replace {
                stage,
                target,
                backup,
            } => {
                path_from_relative(stage)?;
                path_from_relative(target)?;
                path_from_relative(backup)?;
            }
            JournalOperation::Create { stage, target } => {
                path_from_relative(stage)?;
                path_from_relative(target)?;
            }
            JournalOperation::Delete { target, trash } => {
                path_from_relative(target)?;
                path_from_relative(trash)?;
            }
        }
    }
    Ok(())
}

fn checked_join(root: &Path, relative: &str) -> anyhow::Result<PathBuf> {
    Ok(root.join(path_from_relative(relative)?))
}

fn path_from_relative(value: &str) -> anyhow::Result<PathBuf> {
    if value.contains('\\') {
        bail!("journal paths must use forward slashes");
    }
    let path = Path::new(value);
    let mut components = path.components();
    let first = components.next().context("journal path is empty")?;
    match first {
        Component::Normal(value) if value == "agents" || value == "memory" => {}
        _ => bail!("journal path is outside an allowed root"),
    }
    for component in components {
        if !matches!(component, Component::Normal(_)) {
            bail!("journal path contains a non-normal component");
        }
    }
    Ok(path.to_owned())
}

fn validate_leaf_id(id: &str) -> anyhow::Result<()> {
    if id.is_empty() || Path::new(id).components().count() != 1 || id.contains(['/', '\\']) {
        bail!("invalid agent id in disk image");
    }
    Ok(())
}

fn move_path(root: &Path, source: &str, target: &str) -> anyhow::Result<()> {
    let source = checked_join(root, source)?;
    let target = checked_join(root, target)?;
    match (source.exists(), target.exists()) {
        (true, false) => rename_path(&source, &target),
        (false, true) => Ok(()),
        (true, true) => bail!("move has both source and target"),
        (false, false) => bail!("move has neither source nor target"),
    }
}

fn rename_path(source: &Path, target: &Path) -> anyhow::Result<()> {
    if let Some(parent) = target.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::rename(source, target).with_context(|| {
        format!(
            "failed to move {} to {}",
            source.display(),
            target.display()
        )
    })?;
    sync_parent(source)?;
    sync_parent(target)
}

fn remove_path(path: &Path) -> anyhow::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn copy_directory(source: &Path, target: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(target)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let destination = target.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_directory(&entry.path(), &destination)?;
        } else {
            fs::copy(entry.path(), &destination)?;
            File::options().write(true).open(&destination)?.sync_all()?;
        }
    }
    sync_directory(target)
}

fn write_synced(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut file = File::create(path)?;
    file.write_all(bytes)?;
    file.flush()?;
    file.sync_all()?;
    sync_parent(path)
}

fn sync_parent(path: &Path) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        sync_directory(parent)?;
    }
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> anyhow::Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(windows)]
fn sync_directory(_path: &Path) -> anyhow::Result<()> {
    // Windows does not allow opening directories through std::fs::File. File
    // contents are flushed before every rename; rename itself is atomic there.
    Ok(())
}

#[cfg(test)]
mod tests {
    use indexmap::IndexMap;

    use super::*;

    fn image(name: &str) -> RegistryDiskImage {
        RegistryDiskImage {
            index_yaml: "schema_version: 1\norder: [ryuzi]\ndefault_agent_id: ryuzi\n".into(),
            subagents_yaml: "schema_version: 1\nmodel: { route: smart }\n".into(),
            agents: IndexMap::from([("ryuzi".into(), format!("name: {name}\n"))]),
            deleted_agent_ids: Vec::new(),
        }
    }

    #[test]
    fn prepared_transaction_is_recoverable() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("agents/ryuzi")).unwrap();
        fs::write(root.path().join("agents/ryuzi/agent.yaml"), "name: Old\n").unwrap();
        fs::write(root.path().join("agents/index.yaml"), "old index\n").unwrap();
        AgentTransaction::prepare(root.path(), &image("New")).unwrap();
        let notices = recover_transactions(root.path()).unwrap();
        assert_eq!(notices[0].code, "transaction-rolled-back");
        assert_eq!(
            fs::read_to_string(root.path().join("agents/ryuzi/agent.yaml")).unwrap(),
            "name: Old\n"
        );
    }

    #[test]
    fn failure_after_index_replace_rolls_back_every_active_file() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("agents/ryuzi")).unwrap();
        fs::write(root.path().join("agents/ryuzi/agent.yaml"), "name: Old\n").unwrap();
        fs::write(root.path().join("agents/index.yaml"), "old index\n").unwrap();
        fs::write(root.path().join("agents/subagents.yaml"), "old subagents\n").unwrap();
        let tx = AgentTransaction::prepare(root.path(), &image("New"))
            .unwrap()
            .with_failpoint(TransactionFailpoint::AfterIndexReplaceBeforeCommitMarker);
        assert!(tx.commit().is_err());
        assert_eq!(
            fs::read_to_string(root.path().join("agents/ryuzi/agent.yaml")).unwrap(),
            "name: Old\n"
        );
        assert_eq!(
            fs::read_to_string(root.path().join("agents/index.yaml")).unwrap(),
            "old index\n"
        );
    }

    #[test]
    fn committed_journal_is_replayed_and_cleaned() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("agents/ryuzi")).unwrap();
        fs::write(root.path().join("agents/ryuzi/agent.yaml"), "name: Old\n").unwrap();
        fs::write(root.path().join("agents/index.yaml"), "old index\n").unwrap();
        fs::write(root.path().join("agents/subagents.yaml"), "old subagents\n").unwrap();
        let mut tx = AgentTransaction::prepare(root.path(), &image("New")).unwrap();
        tx.commit_before_marker().unwrap();
        tx.journal.phase = JournalPhase::Committed;
        write_journal(&tx.dir, &tx.journal).unwrap();
        drop(tx);
        let notices = recover_transactions(root.path()).unwrap();
        assert_eq!(notices[0].code, "transaction-replayed");
        assert_eq!(
            fs::read_to_string(root.path().join("agents/ryuzi/agent.yaml")).unwrap(),
            "name: New\n"
        );
        assert!(root
            .path()
            .join("agents/.transactions")
            .read_dir()
            .unwrap()
            .next()
            .is_none());
    }

    #[test]
    fn committed_recovery_tolerates_cleanup_that_removed_staging() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("agents/ryuzi")).unwrap();
        fs::write(root.path().join("agents/ryuzi/agent.yaml"), "name: Old\n").unwrap();
        fs::write(root.path().join("agents/index.yaml"), "old index\n").unwrap();
        fs::write(root.path().join("agents/subagents.yaml"), "old subagents\n").unwrap();
        let mut tx = AgentTransaction::prepare(root.path(), &image("New")).unwrap();
        tx.commit_before_marker().unwrap();
        tx.journal.phase = JournalPhase::Committed;
        write_journal(&tx.dir, &tx.journal).unwrap();
        fs::remove_file(checked_join(root.path(), &tx.journal.index_stage).unwrap()).unwrap();
        drop(tx);

        let notices = recover_transactions(root.path()).unwrap();

        assert_eq!(notices[0].code, "transaction-replayed");
        assert_eq!(
            fs::read_to_string(root.path().join("agents/index.yaml")).unwrap(),
            image("New").index_yaml
        );
    }

    #[test]
    fn recovery_rejects_a_journal_for_a_different_transaction_directory() {
        let root = tempfile::tempdir().unwrap();
        fs::create_dir_all(root.path().join("agents/ryuzi")).unwrap();
        fs::write(root.path().join("agents/ryuzi/agent.yaml"), "name: Old\n").unwrap();
        fs::write(root.path().join("agents/index.yaml"), "old index\n").unwrap();
        let tx = AgentTransaction::prepare(root.path(), &image("New")).unwrap();
        let dir = tx.dir.clone();
        drop(tx);
        let mut journal: TransactionJournal =
            serde_json::from_slice(&fs::read(dir.join("journal.json")).unwrap()).unwrap();
        journal.transaction_id = "different".into();
        write_journal(&dir, &journal).unwrap();

        assert!(recover_transactions(root.path()).is_err());
        assert!(dir.exists());
    }

    #[test]
    fn rejects_escaping_journal_paths() {
        for path in [
            "../agents/index.yaml",
            "other/file",
            "agents/../secret",
            "C:/secret",
        ] {
            assert!(path_from_relative(path).is_err(), "accepted {path}");
        }
    }
}
