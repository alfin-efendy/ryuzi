use git2::{Repository, WorktreeAddOptions, WorktreePruneOptions};
use std::path::Path;

/// Create `branch` (cut from `base_branch`'s tip, or the repo HEAD when
/// `None` — the legacy behavior) and add a worktree checked out on it.
pub fn create(
    repo_dir: &Path,
    name: &str,
    branch: &str,
    worktree_path: &Path,
    base_branch: Option<&str>,
) -> Result<(), git2::Error> {
    let repo = Repository::open(repo_dir)?;
    let base_commit = match base_branch {
        Some(base) => repo
            .find_branch(base, git2::BranchType::Local)?
            .get()
            .peel_to_commit()?,
        None => repo.head()?.peel_to_commit()?,
    };
    let branch_ref = repo.branch(branch, &base_commit, false)?;
    let reference = branch_ref.into_reference();

    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(&reference));
    repo.worktree(name, worktree_path, Some(&opts))?;
    Ok(())
}

/// Add a worktree checked out on an EXISTING local branch. Creates no branch.
/// git refuses when the branch is already checked out in the main repo or
/// another worktree — that error propagates verbatim to the caller.
pub fn add_for_branch(
    repo_dir: &Path,
    name: &str,
    branch: &str,
    worktree_path: &Path,
) -> Result<(), git2::Error> {
    let repo = Repository::open(repo_dir)?;
    let branch_ref = repo.find_branch(branch, git2::BranchType::Local)?;
    let reference = branch_ref.into_reference();

    if let Some(parent) = worktree_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }

    let mut opts = WorktreeAddOptions::new();
    opts.reference(Some(&reference));
    repo.worktree(name, worktree_path, Some(&opts))?;
    Ok(())
}

pub fn remove(
    repo_dir: &Path,
    name: &str,
    branch: Option<&str>,
    worktree_path: &Path,
) -> Result<(), git2::Error> {
    // Remove the working directory first so prune (valid=true) can reclaim the entry.
    std::fs::remove_dir_all(worktree_path).ok();
    let repo = Repository::open(repo_dir)?;
    if let Ok(wt) = repo.find_worktree(name) {
        let mut opts = WorktreePruneOptions::new();
        opts.valid(true);
        wt.prune(Some(&mut opts))?;
    }
    // The session branch is engine-owned; delete it once the worktree entry is
    // pruned (git refuses while the branch is still checked out there).
    // Best-effort: a branch the user checked out elsewhere stays put.
    if let Some(branch) = branch {
        if let Ok(mut b) = repo.find_branch(branch, git2::BranchType::Local) {
            b.delete().ok();
        }
    }
    // On Windows a process with its cwd inside the tree (a shell, an indexer)
    // can make the first removal leave the emptied root behind; retry once now
    // that the contents and git entry are gone.
    if worktree_path.exists() {
        std::fs::remove_dir_all(worktree_path).ok();
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
        repo.commit(Some("HEAD"), &sig, &sig, "init", &tree, &[])
            .unwrap();
    }

    #[test]
    fn create_then_remove_worktree() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());

        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("abcdef01");

        create(
            repo_dir.path(),
            "abcdef01",
            "harness/abcdef01",
            &wt_path,
            None,
        )
        .unwrap();
        assert!(wt_path.join(".git").exists());

        let repo = git2::Repository::open(repo_dir.path()).unwrap();
        assert!(repo
            .find_branch("harness/abcdef01", git2::BranchType::Local)
            .is_ok());

        remove(
            repo_dir.path(),
            "abcdef01",
            Some("harness/abcdef01"),
            &wt_path,
        )
        .unwrap();
        assert!(!wt_path.exists());
        // The session branch is engine-owned and must go with the worktree.
        assert!(
            repo.find_branch("harness/abcdef01", git2::BranchType::Local)
                .is_err(),
            "session branch must be deleted with its worktree"
        );
    }

    #[test]
    fn remove_without_branch_name_keeps_branches() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("abcdef02");

        create(
            repo_dir.path(),
            "abcdef02",
            "harness/abcdef02",
            &wt_path,
            None,
        )
        .unwrap();
        remove(repo_dir.path(), "abcdef02", None, &wt_path).unwrap();

        let repo = git2::Repository::open(repo_dir.path()).unwrap();
        assert!(repo
            .find_branch("harness/abcdef02", git2::BranchType::Local)
            .is_ok());
    }

    /// Add `name` with `content` to the index and commit on the current HEAD.
    fn commit_file(repo: &git2::Repository, name: &str, content: &str, msg: &str) {
        let workdir = repo.workdir().unwrap().to_path_buf();
        std::fs::write(workdir.join(name), content).unwrap();
        let mut idx = repo.index().unwrap();
        idx.add_path(Path::new(name)).unwrap();
        idx.write().unwrap();
        let tree_id = idx.write_tree().unwrap();
        let tree = repo.find_tree(tree_id).unwrap();
        let sig = git2::Signature::now("t", "t@t").unwrap();
        let parent = repo.head().ok().and_then(|h| h.peel_to_commit().ok());
        let parents: Vec<&git2::Commit> = parent.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parents)
            .unwrap();
    }

    #[test]
    fn create_from_base_branch_cuts_from_its_tip() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        let repo = git2::Repository::open(repo_dir.path()).unwrap();
        // Branch `feature`, one commit ahead of the default branch.
        let default = repo.head().unwrap().shorthand().unwrap().to_string();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &head, false).unwrap();
        repo.set_head("refs/heads/feature").unwrap();
        commit_file(&repo, "b.txt", "two", "feature work");
        let feature_tip = repo.head().unwrap().peel_to_commit().unwrap().id();
        // Switch back so `feature` is NOT the checked-out branch.
        let obj = repo
            .revparse_single(&format!("refs/heads/{default}"))
            .unwrap();
        repo.checkout_tree(&obj, None).unwrap();
        repo.set_head(&format!("refs/heads/{default}")).unwrap();

        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("abcdef03");
        create(
            repo_dir.path(),
            "abcdef03",
            "harness/abcdef03",
            &wt_path,
            Some("feature"),
        )
        .unwrap();

        let tip = repo
            .find_branch("harness/abcdef03", git2::BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap()
            .id();
        assert_eq!(
            tip, feature_tip,
            "branch must be cut from the base's tip, not HEAD"
        );
    }

    #[test]
    fn add_for_branch_checks_out_existing_branch_without_creating() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        let repo = git2::Repository::open(repo_dir.path()).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &head, false).unwrap();

        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("abcdef04");
        add_for_branch(repo_dir.path(), "abcdef04", "feature", &wt_path).unwrap();

        assert!(wt_path.join(".git").exists());
        // No extra branch was created: default + feature only.
        let count = repo
            .branches(Some(git2::BranchType::Local))
            .unwrap()
            .count();
        assert_eq!(count, 2, "add_for_branch must not create branches");
    }

    #[test]
    fn add_for_branch_fails_when_branch_is_checked_out_in_main_repo() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        let repo = git2::Repository::open(repo_dir.path()).unwrap();
        let current = repo.head().unwrap().shorthand().unwrap().to_string();

        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("abcdef05");
        let err = add_for_branch(repo_dir.path(), "abcdef05", &current, &wt_path);
        assert!(err.is_err(), "git must refuse a branch already checked out");
    }
}
