//! Local-branch listing for the composer's branch picker. Local branches
//! only (remotes are a follow-up); `current` first, then alphabetical.

use git2::{BranchType, Repository};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct BranchList {
    /// Local branch names — current first, then alphabetical.
    pub branches: Vec<String>,
    /// Branch checked out in the project workdir; a short commit id when
    /// HEAD is detached, "HEAD" when the repo has no commits yet.
    pub current: String,
    pub detached: bool,
}

pub fn list_branches(repo_dir: &Path) -> anyhow::Result<BranchList> {
    let repo = Repository::open(repo_dir)
        .map_err(|e| anyhow::anyhow!("not a git repository: {} ({e})", repo_dir.display()))?;

    let (current, detached) = match repo.head() {
        Ok(head) if head.is_branch() => (head.shorthand().unwrap_or("HEAD").to_string(), false),
        Ok(head) => {
            let short = head
                .peel_to_commit()
                .map(|c| c.id().to_string().chars().take(8).collect::<String>())
                .unwrap_or_else(|_| "HEAD".to_string());
            (short, true)
        }
        // Unborn branch (no commits yet): best-effort per the spec.
        Err(_) => ("HEAD".to_string(), true),
    };

    let mut names: Vec<String> = Vec::new();
    for entry in repo.branches(Some(BranchType::Local))? {
        let (branch, _) = entry?;
        if let Some(name) = branch.name()? {
            names.push(name.to_string());
        }
    }
    names.sort();

    let mut branches = Vec::with_capacity(names.len());
    if names.iter().any(|n| n == &current) {
        branches.push(current.clone());
    }
    branches.extend(names.into_iter().filter(|n| n != &current));

    Ok(BranchList {
        branches,
        current,
        detached,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn init_repo(dir: &Path) -> String {
        let repo = Repository::init(dir).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let tree_id = repo.index().unwrap().write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
        let head = repo.head().unwrap();
        let name = head.shorthand().unwrap().to_string();
        name
    }

    #[test]
    fn lists_local_branches_current_first_then_alphabetical() {
        let dir = tempfile::tempdir().unwrap();
        let default = init_repo(dir.path());
        let repo = Repository::open(dir.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("zeta", &head, false).unwrap();
        repo.branch("alpha", &head, false).unwrap();

        let list = list_branches(dir.path()).unwrap();
        assert_eq!(list.current, default);
        assert!(!list.detached);
        assert_eq!(
            list.branches,
            vec![default.clone(), "alpha".to_string(), "zeta".to_string()]
        );
    }

    #[test]
    fn detached_head_reports_short_commit_id() {
        let dir = tempfile::tempdir().unwrap();
        init_repo(dir.path());
        let repo = Repository::open(dir.path()).unwrap();
        let oid = repo.head().unwrap().peel_to_commit().unwrap().id();
        repo.set_head_detached(oid).unwrap();

        let list = list_branches(dir.path()).unwrap();
        assert!(list.detached);
        assert_eq!(
            list.current.len(),
            8,
            "short commit id, got {}",
            list.current
        );
        assert_eq!(list.branches.len(), 1, "the branch itself is still listed");
    }

    #[test]
    fn repo_without_commits_is_best_effort_detached() {
        let dir = tempfile::tempdir().unwrap();
        Repository::init(dir.path()).unwrap();

        let list = list_branches(dir.path()).unwrap();
        assert!(list.detached);
        assert_eq!(list.current, "HEAD");
        assert!(list.branches.is_empty());
    }

    #[test]
    fn non_repo_dir_errors() {
        let dir = tempfile::tempdir().unwrap();
        assert!(list_branches(dir.path()).is_err());
    }
}
