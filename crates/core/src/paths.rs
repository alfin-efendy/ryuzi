use std::path::PathBuf;

pub fn state_dir() -> PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("ryuzi")
}

pub fn db_path() -> PathBuf {
    state_dir().join("ryuzi.sqlite")
}

pub fn worktree_path_for(project_id: &str, session_pk: &str) -> PathBuf {
    let short: String = session_pk.chars().take(8).collect();
    state_dir().join("worktrees").join(project_id).join(short)
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
        let p = worktree_path_for("proj1", "abcdef0123456789");
        assert!(p.ends_with("worktrees/proj1/abcdef01"));
    }

    #[test]
    fn new_id_is_unique_and_hyphenated() {
        let a = new_id();
        let b = new_id();
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
    }
}
