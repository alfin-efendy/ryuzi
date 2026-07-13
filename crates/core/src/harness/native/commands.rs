//! Slash commands for the native runtime.
//!
//! A command is a named prompt template. Built-ins are `/init` (write an
//! AGENTS.md) and `/review` (review the working changes). Custom commands are
//! discovered from markdown files in `.ryuzi/commands/` (project) and
//! `~/.config/ryuzi/commands/` (global). Templates interpolate `$ARGUMENTS`
//! (all args) and `$1`..`$9` (positional).

use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::fmt;
use std::fs::File;
use std::io::Write;
use std::path::{Component, Path, PathBuf};

/// One slash command.
#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub description: String,
    /// The prompt template with `$ARGUMENTS` / `$1`..`$9` placeholders.
    pub template: String,
    /// Optional agent to run this command under.
    pub agent: Option<String>,
    /// Optional model to use for this command's turn.
    pub model: Option<String>,
    /// Whether this command's turn is a subtask.
    pub subtask: bool,
}

/// A slash command expanded for a particular input.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedCommand {
    pub prompt: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub subtask: bool,
}

/// The source of a discovered command file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CommandOrigin {
    Builtin,
    Global,
    Project,
}

impl CommandOrigin {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::Global => "global",
            Self::Project => "project",
        }
    }
}

/// A command file as represented on disk for project command CRUD.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCommandInput {
    pub name: String,
    pub description: String,
    pub template: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub subtask: bool,
}

/// A command file read from a project command directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectCommandRead {
    pub name: String,
    pub description: String,
    pub template: String,
    pub agent: Option<String>,
    pub model: Option<String>,
    pub subtask: bool,
    pub revision: String,
}

/// A normalized, validated project command name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ValidatedCommandName(String);

impl ValidatedCommandName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Errors from safe project command file operations.
#[derive(Debug)]
pub enum CommandFileError {
    InvalidName(String),
    NotFound(String),
    RevisionConflict,
    Io(std::io::Error),
}

impl fmt::Display for CommandFileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidName(message) => write!(f, "invalid command name: {message}"),
            Self::NotFound(name) => write!(f, "project command not found: {name}"),
            Self::RevisionConflict => write!(
                f,
                "project command was modified externally; reload it before saving"
            ),
            Self::Io(error) => error.fmt(f),
        }
    }
}

impl std::error::Error for CommandFileError {}

impl From<std::io::Error> for CommandFileError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

impl Command {
    /// Expand the template with `args` (a single already-split argument string).
    pub fn expand(&self, args: &str) -> String {
        let positional: Vec<&str> = args.split_whitespace().collect();
        let mut out = self.template.replace("$ARGUMENTS", args.trim());
        for (i, p) in positional.iter().enumerate() {
            out = out.replace(&format!("${}", i + 1), p);
        }
        // Unfilled positionals collapse to empty.
        for i in positional.len()..9 {
            out = out.replace(&format!("${}", i + 1), "");
        }
        out
    }
}

fn builtin_commands() -> Vec<Command> {
    vec![
        Command {
            name: "init".into(),
            description: "Analyze the codebase and write an AGENTS.md for future agents.".into(),
            template: "Analyze this codebase (structure, build/test commands, conventions) and \
                       create or update an AGENTS.md at the repository root that a future agent \
                       could use to work effectively here. Keep it concise and concrete. \
                       $ARGUMENTS"
                .into(),
            agent: None,
            model: None,
            subtask: false,
        },
        Command {
            name: "review".into(),
            description: "Review the current working changes for bugs and issues.".into(),
            template: "Review the current working changes (run `git diff` and, if empty, \
                       `git diff --staged`). Report correctness bugs, risky changes, and \
                       obvious improvements, grouped by severity. Do not modify files. \
                       $ARGUMENTS"
                .into(),
            agent: Some("plan".into()),
            model: None,
            subtask: false,
        },
        // /compact is intercepted as an ACTION in runner::run_turn before
        // command resolution — this entry exists only so UIs list it in
        // slash-command autocomplete. Its template is never sent to a model.
        Command {
            name: "compact".into(),
            description: "Summarize older history to free context-window space.".into(),
            template: String::new(),
            agent: None,
            model: None,
            subtask: false,
        },
    ]
}

/// The set of available slash commands.
pub struct CommandRegistry {
    commands: BTreeMap<String, Command>,
}

impl CommandRegistry {
    pub fn load(work_dir: &Path) -> CommandRegistry {
        let global = dirs::home_dir()
            .map(|home| home.join(".config/ryuzi/commands"))
            .unwrap_or_default();
        Self::load_from_dirs(work_dir, &global)
    }

    fn load_from_dirs(work_dir: &Path, global_dir: &Path) -> CommandRegistry {
        let mut commands: BTreeMap<String, Command> = BTreeMap::new();
        for cmd in read_command_dir(global_dir) {
            commands.insert(cmd.name.clone(), cmd);
        }
        for cmd in read_project_command_dir(work_dir) {
            commands.insert(cmd.name.clone(), cmd);
        }
        for cmd in builtin_commands() {
            commands.insert(cmd.name.clone(), cmd);
        }
        CommandRegistry { commands }
    }

    pub fn builtin() -> CommandRegistry {
        CommandRegistry {
            commands: builtin_commands()
                .into_iter()
                .map(|c| (c.name.clone(), c))
                .collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<Command> {
        self.commands.get(name).cloned()
    }

    pub fn names(&self) -> Vec<String> {
        self.commands.keys().cloned().collect()
    }

    /// All commands, for UI listing.
    pub fn all(&self) -> Vec<Command> {
        self.commands.values().cloned().collect()
    }

    /// If `input` is a slash command (`/name args...`), return its expanded
    /// prompt and metadata. Otherwise return `None`.
    pub fn resolve(&self, input: &str) -> Option<ResolvedCommand> {
        let trimmed = input.trim_start();
        let rest = trimmed.strip_prefix('/')?;
        // A bare "/" or "/ foo" is not a command.
        let (name, args) = match rest.split_once(char::is_whitespace) {
            Some((n, a)) => (n, a),
            None => (rest, ""),
        };
        let cmd = self.get(name)?;
        Some(ResolvedCommand {
            prompt: cmd.expand(args),
            agent: cmd.agent,
            model: cmd.model,
            subtask: cmd.subtask,
        })
    }
}

fn project_command_root(
    work_dir: &Path,
    create: bool,
) -> Result<Option<PathBuf>, CommandFileError> {
    let ryuzi_dir = work_dir.join(".ryuzi");
    if !ensure_real_directory(&ryuzi_dir, create, ".ryuzi directory")? {
        return Ok(None);
    }

    let commands_dir = ryuzi_dir.join("commands");
    if !ensure_real_directory(&commands_dir, create, "commands directory")? {
        return Ok(None);
    }
    canonical_command_root(work_dir, &ryuzi_dir, &commands_dir).map(Some)
}

fn ensure_real_directory(
    path: &Path,
    create: bool,
    description: &str,
) -> Result<bool, CommandFileError> {
    match std::fs::symlink_metadata(path) {
        Ok(metadata) => {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(CommandFileError::InvalidName(format!(
                    "{description} must be a real directory"
                )));
            }
            Ok(true)
        }
        Err(error) if error.kind() == std::io::ErrorKind::NotFound && !create => Ok(false),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            match std::fs::create_dir(path) {
                Ok(()) => {}
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => return Err(error.into()),
            }
            ensure_real_directory(path, false, description)
        }
        Err(error) => Err(error.into()),
    }
}

fn read_project_command_dir(work_dir: &Path) -> Vec<Command> {
    let ryuzi_dir = work_dir.join(".ryuzi");
    if ryuzi_dir
        .symlink_metadata()
        .ok()
        .is_some_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Vec::new();
    }

    let commands_dir = ryuzi_dir.join("commands");
    if commands_dir
        .symlink_metadata()
        .ok()
        .is_some_and(|metadata| metadata.file_type().is_symlink() || !metadata.is_dir())
    {
        return Vec::new();
    }
    read_command_dir(&commands_dir)
}

fn read_command_dir(dir: &Path) -> Vec<Command> {
    let mut commands = Vec::new();
    read_command_dir_recursive(dir, dir, &mut commands);
    commands
}

fn read_command_dir_recursive(root: &Path, dir: &Path, commands: &mut Vec<Command>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.filter_map(Result::ok) {
        let path = entry.path();
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            read_command_dir_recursive(root, &path, commands);
            continue;
        }
        if !file_type.is_file() || path.extension().is_none_or(|extension| extension != "md") {
            continue;
        }
        let Some(name) = command_name_from_path(root, &path) else {
            continue;
        };
        let Ok(text) = std::fs::read_to_string(&path) else {
            continue;
        };
        commands.push(parse_command_markdown(&name, &text));
    }
}

fn command_name_from_path(root: &Path, path: &Path) -> Option<String> {
    let relative = path.strip_prefix(root).ok()?;
    let without_extension = relative.with_extension("");
    let name = without_extension
        .components()
        .map(|component| match component {
            Component::Normal(value) => value.to_str(),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()?
        .join("/");
    (!name.is_empty()).then_some(name)
}

/// Validate and normalize a project command name before it is resolved to a path.
pub fn validate_project_command_name(name: &str) -> Result<ValidatedCommandName, CommandFileError> {
    if name.is_empty() || name.len() > 80 {
        return Err(CommandFileError::InvalidName(
            "must contain 1 through 80 bytes".into(),
        ));
    }
    if name.starts_with('/')
        || !name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_' | b'/')
        })
    {
        return Err(CommandFileError::InvalidName(
            "only lowercase letters, digits, '-', '_', and '/' are allowed".into(),
        ));
    }
    if name
        .split('/')
        .any(|segment| segment.is_empty() || matches!(segment, "." | ".."))
    {
        return Err(CommandFileError::InvalidName(
            "path segments must not be empty, '.' or '..'".into(),
        ));
    }
    if matches!(name, "init" | "review" | "compact") {
        return Err(CommandFileError::InvalidName(
            "built-in commands cannot be created or updated".into(),
        ));
    }
    Ok(ValidatedCommandName(name.to_string()))
}

/// List every readable project command file, including its content revision.
pub fn list_project_commands(work_dir: &Path) -> Result<Vec<ProjectCommandRead>, CommandFileError> {
    let Some(root) = project_command_root(work_dir, false)? else {
        return Ok(Vec::new());
    };
    let mut commands = Vec::new();
    list_project_commands_recursive(&root, &root, &mut commands)?;
    commands.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(commands)
}

/// Read one project command by its validated name.
pub fn read_project_command(
    work_dir: &Path,
    name: &str,
) -> Result<ProjectCommandRead, CommandFileError> {
    let name = validate_project_command_name(name)?;
    let Some(root) = project_command_root(work_dir, false)? else {
        return Err(CommandFileError::NotFound(name.0));
    };
    read_project_command_at(&root, &name)
}

/// Atomically create or update a project command file.
pub fn write_project_command(
    work_dir: &Path,
    input: ProjectCommandInput,
    expected_revision: Option<&str>,
) -> Result<ProjectCommandRead, CommandFileError> {
    let name = validate_project_command_name(&input.name)?;
    let root = project_command_root(work_dir, true)?.expect("command root was created");
    let mut lock = command_root_lock(&root)?;
    let _guard = lock.write()?;
    verify_locked_command_root(work_dir, &root)?;
    let path = project_command_path(&root, &name)?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
        reject_symlink_path(&root, parent)?;
    }
    if path.exists() {
        let current = read_project_command_at(&root, &name)?;
        if expected_revision != Some(current.revision.as_str()) {
            return Err(CommandFileError::RevisionConflict);
        }
    } else if expected_revision.is_some() {
        return Err(CommandFileError::RevisionConflict);
    }

    let content = render_project_command(&input);
    let parent = path.parent().expect("command path has a parent");
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    temp.write_all(content.as_bytes())?;
    temp.as_file().sync_all()?;
    temp.persist(&path)
        .map_err(|error| CommandFileError::Io(error.error))?;
    read_project_command_at(&root, &name)
}

/// Delete a project command only when its current content revision matches.
pub fn delete_project_command(
    work_dir: &Path,
    name: &str,
    expected_revision: &str,
) -> Result<(), CommandFileError> {
    let name = validate_project_command_name(name)?;
    let Some(root) = project_command_root(work_dir, false)? else {
        return Err(CommandFileError::NotFound(name.0));
    };
    let mut lock = command_root_lock(&root)?;
    let _guard = lock.write()?;
    verify_locked_command_root(work_dir, &root)?;
    let current = read_project_command_at(&root, &name)?;
    if current.revision != expected_revision {
        return Err(CommandFileError::RevisionConflict);
    }
    let path = project_command_path(&root, &name)?;
    std::fs::remove_file(&path)?;
    remove_empty_command_parents(&root, path.parent());
    Ok(())
}

fn command_root_lock(root: &Path) -> Result<fd_lock::RwLock<File>, CommandFileError> {
    let file = File::options()
        .create(true)
        .truncate(false)
        .read(true)
        .write(true)
        .open(root.join(".commands.lock"))?;
    Ok(fd_lock::RwLock::new(file))
}

fn canonical_command_root(
    work_dir: &Path,
    ryuzi_dir: &Path,
    commands_dir: &Path,
) -> Result<PathBuf, CommandFileError> {
    ensure_real_directory(ryuzi_dir, false, ".ryuzi directory")?;
    ensure_real_directory(commands_dir, false, "commands directory")?;
    let work_dir = work_dir.canonicalize()?;
    let ryuzi_dir = ryuzi_dir.canonicalize()?;
    let commands_dir = commands_dir.canonicalize()?;
    if ryuzi_dir.parent() != Some(work_dir.as_path()) || commands_dir.parent() != Some(&ryuzi_dir) {
        return Err(CommandFileError::InvalidName(
            "command paths escaped the project directory".into(),
        ));
    }
    Ok(commands_dir)
}

fn verify_locked_command_root(work_dir: &Path, root: &Path) -> Result<(), CommandFileError> {
    let ryuzi_dir = work_dir.join(".ryuzi");
    let commands_dir = ryuzi_dir.join("commands");
    let canonical_root = canonical_command_root(work_dir, &ryuzi_dir, &commands_dir)?;
    if canonical_root != root {
        return Err(CommandFileError::InvalidName(
            "commands directory changed while acquiring its lock".into(),
        ));
    }
    Ok(())
}

fn project_command_path(
    root: &Path,
    name: &ValidatedCommandName,
) -> Result<PathBuf, CommandFileError> {
    let path = root.join(name.as_str()).with_extension("md");
    let parent = path.parent().expect("command path has a parent");
    reject_symlink_path(root, parent)?;
    if !path.starts_with(root) {
        return Err(CommandFileError::InvalidName(
            "command path escaped commands directory".into(),
        ));
    }
    if path.exists() && std::fs::symlink_metadata(&path)?.file_type().is_symlink() {
        return Err(CommandFileError::InvalidName(
            "command file must not be a symlink".into(),
        ));
    }
    Ok(path)
}

fn reject_symlink_path(root: &Path, path: &Path) -> Result<(), CommandFileError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        CommandFileError::InvalidName("command path escaped commands directory".into())
    })?;
    let mut current = root.to_path_buf();
    for component in relative.components() {
        let Component::Normal(component) = component else {
            return Err(CommandFileError::InvalidName("invalid command path".into()));
        };
        current.push(component);
        if current.exists()
            && std::fs::symlink_metadata(&current)?
                .file_type()
                .is_symlink()
        {
            return Err(CommandFileError::InvalidName(
                "command paths must not traverse symlinks".into(),
            ));
        }
    }
    Ok(())
}

fn list_project_commands_recursive(
    root: &Path,
    dir: &Path,
    commands: &mut Vec<ProjectCommandRead>,
) -> Result<(), CommandFileError> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_symlink() {
            continue;
        }
        if file_type.is_dir() {
            list_project_commands_recursive(root, &path, commands)?;
        } else if file_type.is_file() && path.extension().is_some_and(|extension| extension == "md")
        {
            if let Some(name) = command_name_from_path(root, &path) {
                if let Ok(name) = validate_project_command_name(&name) {
                    commands.push(read_project_command_at(root, &name)?);
                }
            }
        }
    }
    Ok(())
}

fn read_project_command_at(
    root: &Path,
    name: &ValidatedCommandName,
) -> Result<ProjectCommandRead, CommandFileError> {
    let path = project_command_path(root, name)?;
    if !path.exists() {
        return Err(CommandFileError::NotFound(name.0.clone()));
    }
    let bytes = std::fs::read(&path)?;
    let text = String::from_utf8_lossy(&bytes);
    let command = parse_command_markdown(name.as_str(), &text);
    Ok(ProjectCommandRead {
        name: name.0.clone(),
        description: command.description,
        template: command.template,
        agent: command.agent,
        model: command.model,
        subtask: command.subtask,
        revision: revision(&bytes),
    })
}

fn render_project_command(input: &ProjectCommandInput) -> String {
    let mut frontmatter = format!("---\ndescription: {}\n", input.description);
    if let Some(agent) = input.agent.as_deref() {
        frontmatter.push_str(&format!("agent: {agent}\n"));
    }
    if let Some(model) = input.model.as_deref() {
        frontmatter.push_str(&format!("model: {model}\n"));
    }
    frontmatter.push_str(&format!(
        "subtask: {}\n---\n{}",
        input.subtask, input.template
    ));
    frontmatter
}

fn revision(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn remove_empty_command_parents(root: &Path, mut directory: Option<&Path>) {
    while let Some(dir) = directory {
        if dir == root
            || std::fs::read_dir(dir)
                .ok()
                .and_then(|mut entries| entries.next())
                .is_some()
        {
            break;
        }
        if std::fs::remove_dir(dir).is_err() {
            break;
        }
        directory = dir.parent();
    }
}

fn parse_command_markdown(name: &str, text: &str) -> Command {
    let (frontmatter, body) = super::agents::split_frontmatter_pub(text);
    let mut description = format!("Custom command `/{name}`");
    let mut agent = None;
    let mut model = None;
    let mut subtask = false;
    for (key, value) in frontmatter {
        match key.as_str() {
            "description" => description = value,
            "agent" => agent = Some(value),
            "model" => model = Some(value),
            "subtask" => subtask = matches!(value.trim(), "true" | "TRUE" | "True"),
            _ => {}
        }
    }
    Command {
        name: name.to_string(),
        description,
        template: body,
        agent,
        model,
        subtask,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_present() {
        let reg = CommandRegistry::builtin();
        assert!(reg.get("init").is_some());
        assert!(reg.get("review").is_some());
        assert_eq!(reg.get("review").unwrap().agent.as_deref(), Some("plan"));
        assert!(reg.get("compact").is_some());
    }

    #[test]
    fn resolve_expands_arguments() {
        let reg = CommandRegistry::builtin();
        let resolved = reg.resolve("/review the auth module").unwrap();
        assert!(resolved.prompt.contains("the auth module"));
        assert_eq!(resolved.agent.as_deref(), Some("plan"));
    }

    #[test]
    fn resolve_returns_none_for_plain_text() {
        let reg = CommandRegistry::builtin();
        assert!(reg.resolve("just a normal prompt").is_none());
        assert!(reg.resolve("/unknown-command x").is_none());
    }

    #[test]
    fn expand_fills_positional_and_arguments() {
        let cmd = Command {
            name: "greet".into(),
            description: "d".into(),
            template: "Hello $1, welcome to $2. All: $ARGUMENTS".into(),
            agent: None,
            model: None,
            subtask: false,
        };
        assert_eq!(
            cmd.expand("Alice Wonderland"),
            "Hello Alice, welcome to Wonderland. All: Alice Wonderland"
        );
        // Unfilled positionals collapse.
        assert_eq!(cmd.expand("Bob"), "Hello Bob, welcome to . All: Bob");
    }

    #[test]
    fn discovers_custom_command() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ryuzi/commands")).unwrap();
        std::fs::write(
            dir.path().join(".ryuzi/commands/ship.md"),
            "---\ndescription: Ship it\n---\nRun the release checklist. $ARGUMENTS",
        )
        .unwrap();
        let reg = CommandRegistry::load(dir.path());
        let resolved = reg.resolve("/ship now").unwrap();
        assert!(resolved.prompt.contains("release checklist"));
        assert!(resolved.prompt.contains("now"));
    }

    #[cfg(unix)]
    #[test]
    fn skips_project_commands_when_commands_root_or_ryuzi_ancestor_is_a_symlink() {
        use std::os::unix::fs::symlink;

        for symlinked_ancestor in [false, true] {
            let work_dir = tempfile::tempdir().unwrap();
            let outside = tempfile::tempdir().unwrap();
            let source = if symlinked_ancestor {
                outside.path().join("commands")
            } else {
                outside.path().to_path_buf()
            };
            std::fs::create_dir_all(&source).unwrap();
            std::fs::write(source.join("outside.md"), "outside command").unwrap();

            if symlinked_ancestor {
                symlink(outside.path(), work_dir.path().join(".ryuzi")).unwrap();
            } else {
                std::fs::create_dir(work_dir.path().join(".ryuzi")).unwrap();
                symlink(&source, work_dir.path().join(".ryuzi/commands")).unwrap();
            }

            let registry = CommandRegistry::load(work_dir.path());
            assert!(
                registry.get("outside").is_none(),
                "must not discover a command outside the project through a symlinked root"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn project_command_crud_rejects_symlinked_ryuzi_ancestor_without_mutating_outside() {
        use std::os::unix::fs::symlink;

        let work_dir = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_commands = outside.path().join("commands");
        let outside_command = outside_commands.join("ship.md");
        let outside_content = "---\ndescription: Outside\n---\nDo not change";
        std::fs::create_dir_all(&outside_commands).unwrap();
        std::fs::write(&outside_command, outside_content).unwrap();
        symlink(outside.path(), work_dir.path().join(".ryuzi")).unwrap();

        let input = ProjectCommandInput {
            name: "new-command".into(),
            description: "Must not be created outside the project".into(),
            template: "Do not write".into(),
            agent: None,
            model: None,
            subtask: false,
        };

        let listed = list_project_commands(work_dir.path());
        let read = read_project_command(work_dir.path(), "ship");
        let write = write_project_command(work_dir.path(), input, None);
        let deleted = delete_project_command(
            work_dir.path(),
            "ship",
            &revision(outside_content.as_bytes()),
        );

        assert!(listed.is_err(), "listing must reject a symlinked .ryuzi");
        assert!(read.is_err(), "reading must reject a symlinked .ryuzi");
        assert!(write.is_err(), "writing must reject a symlinked .ryuzi");
        assert!(deleted.is_err(), "deleting must reject a symlinked .ryuzi");
        assert_eq!(
            std::fs::read_to_string(&outside_command).unwrap(),
            outside_content
        );
        assert!(
            !outside_commands.join("new-command.md").exists(),
            "writing through a symlinked .ryuzi must not create outside files"
        );
    }

    #[test]
    fn reads_nested_command_and_optional_model_metadata() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(".ryuzi/commands/review/security.md");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            "---\ndescription: Secure\nagent: plan\nmodel: openai/gpt-4.1\nsubtask: true\n---\nReview $ARGUMENTS",
        )
        .unwrap();

        let cmd = CommandRegistry::load(dir.path())
            .get("review/security")
            .unwrap();
        assert_eq!(cmd.description, "Secure");
        assert_eq!(cmd.agent.as_deref(), Some("plan"));
        assert_eq!(cmd.model.as_deref(), Some("openai/gpt-4.1"));
        assert!(cmd.subtask);
    }

    #[test]
    fn builtins_win_and_project_overrides_global() {
        let dir = tempfile::tempdir().unwrap();
        let global = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ryuzi/commands")).unwrap();
        std::fs::write(dir.path().join(".ryuzi/commands/init.md"), "project init").unwrap();
        std::fs::write(dir.path().join(".ryuzi/commands/ship.md"), "project ship").unwrap();
        std::fs::write(global.path().join("init.md"), "global init").unwrap();
        std::fs::write(global.path().join("ship.md"), "global ship").unwrap();

        let registry = CommandRegistry::load_from_dirs(dir.path(), global.path());
        assert!(registry
            .get("init")
            .unwrap()
            .template
            .contains("Analyze this codebase"));
        assert_eq!(registry.get("ship").unwrap().template, "project ship");
    }

    #[test]
    fn validates_command_names_and_rejects_path_escapes() {
        for name in [
            "",
            "/ship",
            "ship//now",
            "ship/./now",
            "ship/../now",
            "UPPER",
            "init",
        ] {
            assert!(validate_project_command_name(name).is_err(), "{name}");
        }
        assert!(validate_project_command_name("review/security-2_ok").is_ok());
    }

    #[test]
    fn writes_atomically_and_rejects_stale_revision() {
        let dir = tempfile::tempdir().unwrap();
        let input = ProjectCommandInput {
            name: "review/security".into(),
            description: "Security review".into(),
            template: "Review $ARGUMENTS".into(),
            agent: Some("plan".into()),
            model: Some("openai/gpt-4.1".into()),
            subtask: true,
        };
        let created = write_project_command(dir.path(), input.clone(), None).unwrap();
        assert_eq!(created.revision.len(), 64);
        assert_eq!(created.name, "review/security");

        let error = write_project_command(
            dir.path(),
            ProjectCommandInput {
                template: "changed".into(),
                ..input
            },
            Some("stale"),
        )
        .unwrap_err();
        assert!(matches!(error, CommandFileError::RevisionConflict));
    }

    #[test]
    fn command_root_lock_excludes_a_concurrent_mutator_until_released() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join(".ryuzi/commands");
        std::fs::create_dir_all(&root).unwrap();

        let mut held = command_root_lock(&root).unwrap();
        let guard = held.try_write().unwrap();
        let mut contender = command_root_lock(&root).unwrap();
        assert!(
            contender.try_write().is_err(),
            "a mutation must wait for the root lock before checking a revision"
        );
        drop(guard);
        assert!(
            contender.try_write().is_ok(),
            "the mutation lock must be released when the prior mutation finishes"
        );
    }

    #[test]
    fn legacy_command_files_default_new_metadata() {
        let command =
            parse_command_markdown("ship", "---\ndescription: Ship\nagent: plan\n---\nShip");
        assert_eq!(command.model, None);
        assert!(!command.subtask);
    }

    #[test]
    fn resolve_keeps_agent_model_and_subtask_overrides() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ryuzi/commands")).unwrap();
        std::fs::write(
            dir.path().join(".ryuzi/commands/ship.md"),
            "---\nagent: plan\nmodel: openai/gpt-4.1\nsubtask: true\n---\nShip $ARGUMENTS",
        )
        .unwrap();
        let resolved = CommandRegistry::load(dir.path())
            .resolve("/ship today")
            .unwrap();
        assert_eq!(resolved.prompt, "Ship today");
        assert_eq!(resolved.agent.as_deref(), Some("plan"));
        assert_eq!(resolved.model.as_deref(), Some("openai/gpt-4.1"));
        assert!(resolved.subtask);
    }
}
