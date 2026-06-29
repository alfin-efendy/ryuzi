use git2::{Repository, WorktreeAddOptions, WorktreePruneOptions};
use std::path::Path;

pub fn create(
    repo_dir: &Path,
    name: &str,
    branch: &str,
    worktree_path: &Path,
) -> Result<(), git2::Error> {
    let repo = Repository::open(repo_dir)?;
    let head_commit = repo.head()?.peel_to_commit()?;
    let branch_ref = repo.branch(branch, &head_commit, false)?;
    let reference = branch_ref.into_reference();

    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(&reference));
    repo.worktree(name, worktree_path, Some(&opts))?;
    Ok(())
}

pub fn remove(repo_dir: &Path, name: &str, worktree_path: &Path) -> Result<(), git2::Error> {
    // Remove the working directory first so prune (valid=true) can reclaim the entry.
    std::fs::remove_dir_all(worktree_path).ok();
    let repo = Repository::open(repo_dir)?;
    if let Ok(wt) = repo.find_worktree(name) {
        let mut opts = WorktreePruneOptions::new();
        opts.valid(true);
        wt.prune(Some(&mut opts))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    /// Init a repo at `dir` with a single empty commit on `main`.
    fn init_repo(dir: &Path) {
        let repo = git2::Repository::init(dir).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let tree_id = {
            let mut idx = repo.index().unwrap();
            idx.write_tree().unwrap()
        };
        let tree = repo.find_tree(tree_id).unwrap();
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[]).unwrap();
    }

    #[test]
    fn create_then_remove_worktree() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());

        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("abcdef01");

        create(repo_dir.path(), "abcdef01", "harness/abcdef01", &wt_path).unwrap();
        assert!(wt_path.join(".git").exists());

        let repo = git2::Repository::open(repo_dir.path()).unwrap();
        assert!(repo.find_branch("harness/abcdef01", git2::BranchType::Local).is_ok());

        remove(repo_dir.path(), "abcdef01", &wt_path).unwrap();
        assert!(!wt_path.exists());
    }
}
