//! Agent definitions for the native runtime.
//!
//! An agent bundles a system prompt, a tool allow-list, and a mode (primary vs
//! subagent), mirroring opencode's agent model. Built-ins are `build`, `plan`,
//! `general`, and `explore`; custom agents are discovered from markdown files
//! with frontmatter in `.ryuzi/agents/` (project) and
//! `~/.config/ryuzi/agents/` (global).

use std::collections::BTreeMap;
use std::path::Path;

/// Where an agent may be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentMode {
    /// A top-level agent the user selects for a session.
    Primary,
    /// Only spawnable as a sub-agent via the `task` tool.
    Subagent,
    /// Usable either way.
    All,
}

impl AgentMode {
    fn parse(s: &str) -> AgentMode {
        match s.trim() {
            "subagent" => AgentMode::Subagent,
            "all" => AgentMode::All,
            _ => AgentMode::Primary,
        }
    }
    pub fn is_primary(self) -> bool {
        matches!(self, AgentMode::Primary | AgentMode::All)
    }
    pub fn is_subagent(self) -> bool {
        matches!(self, AgentMode::Subagent | AgentMode::All)
    }
    pub fn as_str(self) -> &'static str {
        match self {
            AgentMode::Primary => "primary",
            AgentMode::Subagent => "subagent",
            AgentMode::All => "all",
        }
    }
}

/// Which tools an agent may call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolFilter {
    /// All registered tools.
    All,
    /// Only these tool names.
    Only(Vec<String>),
}

impl ToolFilter {
    /// Whether `tool` is permitted for this agent.
    pub fn allows(&self, tool: &str) -> bool {
        match self {
            ToolFilter::All => true,
            ToolFilter::Only(list) => list.iter().any(|t| t == tool),
        }
    }
}

/// One agent definition.
#[derive(Debug, Clone)]
pub struct Agent {
    pub name: String,
    pub description: String,
    pub mode: AgentMode,
    /// Custom system prompt; `None` uses the runtime's assembled prompt.
    pub prompt: Option<String>,
    pub tools: ToolFilter,
    /// Whether this agent, when spawned as a sub-agent, may itself delegate
    /// via the `task` tool (subject to the `max_spawn_depth` setting).
    /// Frontmatter key: `delegate: true`.
    pub can_delegate: bool,
    /// Built-in agents cannot be removed by config.
    pub builtin: bool,
}

const READ_ONLY_TOOLS: &[&str] = &["read", "ls", "glob", "grep", "webfetch", "todoread"];

const ORCHESTRATOR_PROMPT: &str = "\
You are an orchestrator sub-agent. Break the given goal into 2-6 \
self-contained subtasks and run them with the `task` tool — use the batch \
form (`tasks: [...]`) for independent subtasks so they run in parallel, and \
sequential single calls when one subtask feeds the next. Choose exactly one \
form per `task` call: never include a top-level `prompt` alongside `tasks`. \
Sub-agents cannot \
see your conversation, so every prompt must carry all needed context. Do small \
connective work yourself instead of delegating it. Finish with one \
synthesized report of what was done, found, or failed.";

fn builtin_agents() -> Vec<Agent> {
    vec![
        Agent {
            name: "build".into(),
            description: "Full-access engineering agent: reads, edits, runs commands.".into(),
            mode: AgentMode::Primary,
            prompt: None,
            tools: ToolFilter::All,
            can_delegate: false,
            builtin: true,
        },
        Agent {
            name: "plan".into(),
            description:
                "Read-only planner: investigates and proposes, never edits or runs commands.".into(),
            mode: AgentMode::Primary,
            prompt: Some(
                "You are in PLAN mode. Investigate the codebase and produce a concrete plan. \
                 You may read, search, and fetch, but you must NOT edit files or run commands. \
                 Present the plan as clear, ordered steps."
                    .into(),
            ),
            tools: ToolFilter::Only(READ_ONLY_TOOLS.iter().map(|s| s.to_string()).collect()),
            can_delegate: false,
            builtin: true,
        },
        Agent {
            name: "general".into(),
            description: "General-purpose sub-agent for multi-step research and edits.".into(),
            mode: AgentMode::Subagent,
            prompt: None,
            tools: ToolFilter::All,
            can_delegate: false,
            builtin: true,
        },
        Agent {
            name: "explore".into(),
            description: "Read-only sub-agent for locating code and answering questions.".into(),
            mode: AgentMode::Subagent,
            prompt: Some(
                "You are a read-only exploration sub-agent. Locate relevant code and report \
                 concise findings with file paths. Do not edit files or run commands."
                    .into(),
            ),
            tools: ToolFilter::Only(
                ["read", "ls", "glob", "grep"]
                    .iter()
                    .map(|s| s.to_string())
                    .collect(),
            ),
            can_delegate: false,
            builtin: true,
        },
        Agent {
            name: "orchestrator".into(),
            description: "Coordinator sub-agent: decomposes a wide goal and runs task batches."
                .into(),
            mode: AgentMode::Subagent,
            prompt: Some(ORCHESTRATOR_PROMPT.into()),
            tools: ToolFilter::All,
            can_delegate: true,
            builtin: true,
        },
    ]
}

/// The set of available agents (built-ins plus discovered custom agents).
pub struct AgentRegistry {
    agents: BTreeMap<String, Agent>,
}

impl AgentRegistry {
    /// Built-ins plus custom agents discovered under `work_dir` and the global
    /// config dir. Custom agents may override built-ins by name.
    pub fn load(work_dir: &Path) -> AgentRegistry {
        let mut agents: BTreeMap<String, Agent> = builtin_agents()
            .into_iter()
            .map(|a| (a.name.clone(), a))
            .collect();
        for dir in agent_dirs(work_dir) {
            for agent in read_agent_dir(&dir) {
                agents.insert(agent.name.clone(), agent);
            }
        }
        AgentRegistry { agents }
    }

    /// Built-ins only (no filesystem discovery) — for tests and defaults.
    pub fn builtin() -> AgentRegistry {
        AgentRegistry {
            agents: builtin_agents()
                .into_iter()
                .map(|a| (a.name.clone(), a))
                .collect(),
        }
    }

    pub fn get(&self, name: &str) -> Option<Agent> {
        self.agents.get(name).cloned()
    }

    /// The default primary agent (`build`).
    pub fn default_agent(&self) -> Agent {
        self.get("build")
            .unwrap_or_else(|| builtin_agents().remove(0))
    }

    /// Agents usable as sub-agents, for the `task` tool description.
    pub fn subagents(&self) -> Vec<Agent> {
        self.agents
            .values()
            .filter(|a| a.mode.is_subagent())
            .cloned()
            .collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.agents.keys().cloned().collect()
    }

    /// All agents, for UI listing.
    pub fn all(&self) -> Vec<Agent> {
        self.agents.values().cloned().collect()
    }
}

fn agent_dirs(work_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![work_dir.join(".ryuzi/agents")];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".config/ryuzi/agents"));
    }
    dirs
}

fn read_agent_dir(dir: &Path) -> Vec<Agent> {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return vec![];
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "md"))
        .filter_map(|e| {
            let name = e.path().file_stem()?.to_string_lossy().to_string();
            let text = std::fs::read_to_string(e.path()).ok()?;
            Some(parse_agent_markdown(&name, &text))
        })
        .collect()
}

/// Parse a `--- key: value ---` frontmatter block plus a markdown body into an
/// [`Agent`]. Recognized keys: `description`, `mode`, `tools` (comma list),
/// `delegate` (true|false).
fn parse_agent_markdown(name: &str, text: &str) -> Agent {
    let (frontmatter, body) = split_frontmatter(text);
    let mut description = format!("Custom agent `{name}`");
    let mut mode = AgentMode::All;
    let mut tools = ToolFilter::All;
    let mut can_delegate = false;
    for (key, value) in frontmatter {
        match key.as_str() {
            "description" => description = value,
            "mode" => mode = AgentMode::parse(&value),
            "delegate" => can_delegate = value.trim().eq_ignore_ascii_case("true"),
            "tools" => {
                let list: Vec<String> = value
                    .split(',')
                    .map(|s| s.trim().to_string())
                    .filter(|s| !s.is_empty())
                    .collect();
                if !list.is_empty() {
                    tools = ToolFilter::Only(list);
                }
            }
            _ => {}
        }
    }
    let prompt = body.trim();
    Agent {
        name: name.to_string(),
        description,
        mode,
        prompt: (!prompt.is_empty()).then(|| prompt.to_string()),
        tools,
        can_delegate,
        builtin: false,
    }
}

/// Crate-visible wrapper over [`split_frontmatter`], shared with the commands
/// module so both parse the same `--- key: value ---` header format.
pub(crate) fn split_frontmatter_pub(text: &str) -> (Vec<(String, String)>, String) {
    split_frontmatter(text)
}

/// Split leading `---\n...\n---\n` frontmatter (simple `key: value` lines) from
/// the body. Returns `(pairs, body)`.
fn split_frontmatter(text: &str) -> (Vec<(String, String)>, String) {
    let trimmed = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = trimmed.strip_prefix("---") else {
        return (vec![], text.to_string());
    };
    let rest = rest.trim_start_matches(['\n', '\r']);
    let Some(end) = rest.find("\n---") else {
        return (vec![], text.to_string());
    };
    let (fm, body) = rest.split_at(end);
    let pairs = fm
        .lines()
        .filter_map(|line| {
            let (k, v) = line.split_once(':')?;
            Some((k.trim().to_lowercase(), v.trim().to_string()))
        })
        .collect();
    let body = body
        .trim_start_matches("\n---")
        .trim_start_matches(['\n', '\r'])
        .to_string();
    (pairs, body)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn builtins_present_with_expected_modes_and_filters() {
        let reg = AgentRegistry::builtin();
        assert_eq!(reg.default_agent().name, "build");
        assert!(reg.get("build").unwrap().tools.allows("bash"));
        // plan is read-only.
        let plan = reg.get("plan").unwrap();
        assert!(plan.tools.allows("read"));
        assert!(!plan.tools.allows("write"));
        assert!(!plan.tools.allows("bash"));
        assert!(plan.prompt.is_some());
        // subagents.
        let subs: Vec<_> = reg.subagents().into_iter().map(|a| a.name).collect();
        assert!(subs.contains(&"general".to_string()));
        assert!(subs.contains(&"explore".to_string()));
        assert!(!subs.contains(&"build".to_string()));
    }

    #[test]
    fn parse_markdown_agent_reads_frontmatter_and_body() {
        let md = "---\ndescription: Docs writer\nmode: subagent\ntools: read, write\n---\nYou write documentation.";
        let a = parse_agent_markdown("docs", md);
        assert_eq!(a.description, "Docs writer");
        assert_eq!(a.mode, AgentMode::Subagent);
        assert!(a.tools.allows("read") && a.tools.allows("write"));
        assert!(!a.tools.allows("bash"));
        assert_eq!(a.prompt.as_deref(), Some("You write documentation."));
        assert!(!a.can_delegate, "delegate defaults to false");
        assert!(!a.builtin);
    }

    #[test]
    fn parse_delegate_frontmatter_flag() {
        let a = parse_agent_markdown(
            "lead",
            "---\nmode: subagent\ndelegate: true\n---\nCoordinate the team.",
        );
        assert!(a.can_delegate);
        let a = parse_agent_markdown("lead2", "---\ndelegate: nope\n---\nx");
        assert!(!a.can_delegate);
    }

    #[test]
    fn orchestrator_builtin_is_a_delegating_subagent() {
        let reg = AgentRegistry::builtin();
        let orch = reg.get("orchestrator").unwrap();
        assert!(orch.mode.is_subagent());
        assert!(!orch.mode.is_primary());
        assert!(orch.can_delegate);
        assert!(orch.tools.allows("task") && orch.tools.allows("bash"));
        assert!(orch.prompt.as_deref().unwrap().contains("batch form"));
        assert!(orch.prompt.as_deref().unwrap().contains("exactly one form"));
        // No other builtin delegates.
        for name in ["build", "plan", "general", "explore"] {
            assert!(!reg.get(name).unwrap().can_delegate, "{name}");
        }
    }

    #[test]
    fn discovers_custom_agent_from_project_dir() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join(".ryuzi/agents")).unwrap();
        std::fs::write(
            dir.path().join(".ryuzi/agents/reviewer.md"),
            "---\ndescription: Reviews code\nmode: subagent\n---\nReview carefully.",
        )
        .unwrap();
        let reg = AgentRegistry::load(dir.path());
        let r = reg.get("reviewer").unwrap();
        assert_eq!(r.description, "Reviews code");
        assert_eq!(r.mode, AgentMode::Subagent);
        // Built-ins still present.
        assert!(reg.get("build").is_some());
    }

    #[test]
    fn body_without_frontmatter_is_all_prompt() {
        let a = parse_agent_markdown("x", "Just a prompt, no frontmatter.");
        assert_eq!(a.prompt.as_deref(), Some("Just a prompt, no frontmatter."));
        assert_eq!(a.mode, AgentMode::All);
    }
}
