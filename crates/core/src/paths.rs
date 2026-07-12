use std::path::{Path, PathBuf};

pub fn state_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ryuzi")
}

pub fn config_dir() -> PathBuf {
    dirs::config_dir()
        .unwrap_or_else(std::env::temp_dir)
        .join("ryuzi")
}

pub fn agents_dir() -> PathBuf {
    agents_dir_in(&config_dir())
}

pub fn agents_dir_in(config_root: &Path) -> PathBuf {
    config_root.join("agents")
}

pub fn agent_dir_in(config_root: &Path, agent_id: &str) -> PathBuf {
    agents_dir_in(config_root).join(agent_id)
}

pub fn agent_knowledge_dir_in(config_root: &Path, agent_id: &str) -> PathBuf {
    agent_dir_in(config_root, agent_id).join("knowledge")
}

pub fn db_path() -> PathBuf {
    state_dir().join("ryuzi.sqlite")
}

/// Base directory a git session's isolated worktree is created under.
/// `base` overrides the default `state_dir()/worktrees` root — pass the
/// resolved `worktree_dir` setting (already `expand_home`-d), or `None` to
/// fall back to the default.
pub fn worktree_path_for(base: Option<&Path>, project_id: &str, session_pk: &str) -> PathBuf {
    let short: String = session_pk.chars().take(8).collect();
    let root = base
        .map(Path::to_path_buf)
        .unwrap_or_else(|| state_dir().join("worktrees"));
    root.join(project_id).join(short)
}

/// Managed scratch working directory for a project-less `chat` session.
/// Lives under the state dir (never `$HOME`), created on first resolve.
pub fn chat_scratch_dir(session_pk: &str) -> PathBuf {
    state_dir().join("chat").join(session_pk)
}

pub fn new_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn state_dir_is_under_ryuzi() {
        assert!(state_dir().ends_with("ryuzi"));
        assert!(db_path().ends_with("ryuzi.sqlite"));
    }

    #[test]
    fn worktree_path_uses_short_session_id() {
        let p = worktree_path_for(None, "proj1", "abcdef0123456789");
        assert!(p.ends_with("worktrees/proj1/abcdef01"));
    }

    #[test]
    fn worktree_path_honors_custom_base() {
        let base = PathBuf::from("/custom/wt-root");
        let p = worktree_path_for(Some(&base), "proj1", "abcdef0123456789");
        assert_eq!(p, PathBuf::from("/custom/wt-root/proj1/abcdef01"));
    }

    #[test]
    fn agent_paths_use_config_not_state_root() {
        let root = PathBuf::from("config-root");
        assert_eq!(agents_dir_in(&root), root.join("agents"));
        assert_eq!(
            agent_dir_in(&root, "reviewer"),
            root.join("agents/reviewer")
        );
        assert_eq!(
            agent_knowledge_dir_in(&root, "reviewer"),
            root.join("agents/reviewer/knowledge")
        );
    }

    #[test]
    fn new_id_is_unique_and_hyphenated() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
    }
}
