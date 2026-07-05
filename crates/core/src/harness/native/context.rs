//! System-context assembly for the native runtime.
//!
//! Builds the Anthropic `system` string each turn from the base ryuzi agent
//! prompt plus discovered instruction files (`AGENTS.md` / `CLAUDE.md`),
//! mirroring opencode's instruction model (simplified for Phase 1: rebuilt per
//! turn, no context epochs).

use std::path::Path;

const BASE_PROMPT: &str = "\
You are ryuzi, an autonomous software engineering agent running in a native \
Rust runtime. You operate inside a git worktree and act by calling tools.

Guidelines:
- Prefer the provided tools (read, ls, glob, grep, edit, write, bash, \
todowrite, webfetch) over guessing. Inspect files before editing them.
- Make the smallest change that satisfies the request; match existing style.
- Use `edit` with a unique `old_string` for precise changes; use `write` only \
for new files or full rewrites.
- Use `bash` for builds, tests, and git; keep commands scoped to the worktree.
- For multi-step work, keep a plan with `todowrite` and update it as you go.
- When the task is complete, stop and summarize what you did. Do not ask for \
confirmation to proceed with reversible work.";

/// Assemble the system prompt for a session rooted at `work_dir`.
/// `memory` is the persistent-memory snapshot to inject (primary agents on
/// the assembled prompt only — agents with custom prompts and sub-agents
/// run memoryless).
pub fn assemble_system(work_dir: &Path, memory: Option<&str>) -> String {
    let mut sections: Vec<String> = vec![BASE_PROMPT.to_string()];

    // Environment facts.
    sections.push(format!(
        "Environment:\n- Working directory: {}\n- Platform: {}",
        work_dir.display(),
        std::env::consts::OS
    ));

    // Global instruction files.
    if let Some(home) = dirs::home_dir() {
        push_if_present(&mut sections, &home.join(".config/ryuzi/AGENTS.md"));
        push_if_present(&mut sections, &home.join(".claude/CLAUDE.md"));
    }

    // Project instruction files, walked from the worktree up to the fs root,
    // nearest-last so the most specific instructions come last.
    let mut dirs_up: Vec<&Path> = Vec::new();
    let mut cur = Some(work_dir);
    while let Some(d) = cur {
        dirs_up.push(d);
        cur = d.parent();
    }
    for dir in dirs_up.into_iter().rev() {
        push_if_present(&mut sections, &dir.join("AGENTS.md"));
        push_if_present(&mut sections, &dir.join("CLAUDE.md"));
    }

    // Persistent memory snapshot (before skills so remembered conventions
    // precede tooling hints).
    if let Some(mem) = memory {
        let mem = mem.trim();
        if !mem.is_empty() {
            sections.push(mem.to_string());
        }
    }

    // Available skills (names + descriptions only; bodies load via the tool).
    if let Some(guidance) = super::skills::SkillRegistry::load(work_dir).guidance() {
        sections.push(guidance);
    }

    sections.join("\n\n")
}

fn push_if_present(sections: &mut Vec<String>, path: &Path) {
    if let Ok(text) = std::fs::read_to_string(path) {
        let text = text.trim();
        if !text.is_empty() {
            sections.push(format!("# Instructions from {}\n\n{text}", path.display()));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn includes_base_prompt_and_environment() {
        let dir = tempfile::tempdir().unwrap();
        let sys = assemble_system(dir.path(), None);
        assert!(sys.contains("You are ryuzi"));
        assert!(sys.contains("Working directory"));
        assert!(sys.contains(&dir.path().display().to_string()));
    }

    #[test]
    fn includes_project_agents_md() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("AGENTS.md"), "Follow the house style.").unwrap();
        let sys = assemble_system(dir.path(), None);
        assert!(sys.contains("Follow the house style."));
        assert!(sys.contains("Instructions from"));
    }

    #[test]
    fn injects_memory_snapshot_when_present() {
        let dir = tempfile::tempdir().unwrap();
        let sys = assemble_system(
            dir.path(),
            Some("# Persistent memory (global) [1% full — 11/6000 chars]\nglobal fact"),
        );
        assert!(sys.contains("# Persistent memory (global)"));
        assert!(sys.contains("global fact"));
        // Empty snapshots add nothing.
        let sys = assemble_system(dir.path(), Some("   "));
        assert!(!sys.contains("Persistent memory"));
    }
}
