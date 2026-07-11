use std::path::{Path, PathBuf};

pub fn state_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ryuzi")
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
    fn new_id_is_unique_and_hyphenated() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
    }
}
