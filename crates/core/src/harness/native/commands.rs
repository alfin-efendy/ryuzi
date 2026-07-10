//! Slash commands for the native runtime.
//!
//! A command is a named prompt template. Built-ins are `/init` (write an
//! AGENTS.md) and `/review` (review the working changes). Custom commands are
//! discovered from markdown files in `.ryuzi/commands/` (project) and
//! `~/.config/ryuzi/commands/` (global). Templates interpolate `$ARGUMENTS`
//! (all args) and `$1`..`$9` (positional).

use std::collections::BTreeMap;
use std::path::Path;

/// One slash command.
#[derive(Debug, Clone)]
pub struct Command {
    pub name: String,
    pub description: String,
    /// The prompt template with `$ARGUMENTS` / `$1`..`$9` placeholders.
    pub template: String,
    /// Optional agent to run this command under.
    pub agent: Option<String>,
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
        },
        // /compact is intercepted as an ACTION in runner::run_turn before
        // command resolution — this entry exists only so UIs list it in
        // slash-command autocomplete. Its template is never sent to a model.
        Command {
            name: "compact".into(),
            description: "Summarize older history to free context-window space.".into(),
            template: String::new(),
            agent: None,
        },
    ]
}

/// The set of available slash commands.
pub struct CommandRegistry {
    commands: BTreeMap<String, Command>,
}

impl CommandRegistry {
    pub fn load(work_dir: &Path) -> CommandRegistry {
        let mut commands: BTreeMap<String, Command> = builtin_commands()
            .into_iter()
            .map(|c| (c.name.clone(), c))
            .collect();
        for dir in command_dirs(work_dir) {
            for cmd in read_command_dir(&dir) {
                commands.insert(cmd.name.clone(), cmd);
            }
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

    /// If `input` is a slash command (`/name args...`), return the resolved
    /// `(expanded_prompt, agent_override)`. Otherwise `None`.
    pub fn resolve(&self, input: &str) -> Option<(String, Option<String>)> {
        let trimmed = input.trim_start();
        let rest = trimmed.strip_prefix('/')?;
        // A bare "/" or "/ foo" is not a command.
        let (name, args) = match rest.split_once(char::is_whitespace) {
            Some((n, a)) => (n, a),
            None => (rest, ""),
        };
        let cmd = self.get(name)?;
        Some((cmd.expand(args), cmd.agent.clone()))
    }
}

fn command_dirs(work_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![work_dir.join(".ryuzi/commands")];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".config/ryuzi/commands"));
    }
    dirs
}

fn read_command_dir(dir: &Path) -> Vec<Command> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter_map(|e| {
            let name = e.path().file_stem()?.to_string_lossy().to_string();
            let text = std::fs::read_to_string(e.path()).ok()?;
            Some(parse_command_markdown(&name, &text))
        })
        .collect()
}

fn parse_command_markdown(name: &str, text: &str) -> Command {
    let (frontmatter, body) = super::agents::split_frontmatter_pub(text);
    let mut description = format!("Custom command `/{name}`");
    let mut agent = None;
    for (key, value) in frontmatter {
        match key.as_str() {
            "description" => description = value,
            "agent" => agent = Some(value),
            _ => {}
        }
    }
    Command {
        name: name.to_string(),
        description,
        template: body,
        agent,
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
        let (prompt, agent) = reg.resolve("/review the auth module").unwrap();
        assert!(prompt.contains("the auth module"));
        assert_eq!(agent.as_deref(), Some("plan"));
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
        let (prompt, _) = reg.resolve("/ship now").unwrap();
        assert!(prompt.contains("release checklist"));
        assert!(prompt.contains("now"));
    }
}
