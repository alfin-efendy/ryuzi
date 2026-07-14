//! Prepares a session's git workspace per the branch-controls behavior
//! matrix (use_worktree × create_branch). Pure git2 — every failure returns
//! BEFORE the caller inserts a Session row.

use crate::domain::SessionGitOptions;
use crate::worktree;
use git2::{BranchType, Repository, StatusOptions};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreparedWorkspace {
    /// Directory the harness session runs in.
    pub work_dir: PathBuf,
    /// Branch the session runs on (recorded on the Session row and shown in UI).
    pub branch: String,
    /// `Some` only when an isolated worktree was created.
    pub worktree_path: Option<PathBuf>,
    /// True when the engine generated the branch name — teardown may delete it.
    pub branch_owned: bool,
}

pub fn prepare_session_workspace(
    repo_dir: &Path,
    git: &SessionGitOptions,
    session_pk: &str,
    worktree_path: &Path,
) -> anyhow::Result<PreparedWorkspace> {
    let short: String = session_pk.chars().take(8).collect();
    let repo = Repository::open(repo_dir)
        .map_err(|e| anyhow::anyhow!("not a git repository: {} ({e})", repo_dir.display()))?;

    // Validate up front so every failure happens before any git mutation.
    if let Some(name) = git.branch_name.as_deref() {
        validate_branch_name(name)?;
    }
    if let Some(base) = git.base_branch.as_deref() {
        if repo.find_branch(base, BranchType::Local).is_err() {
            anyhow::bail!("base branch '{base}' does not exist");
        }
    }
    let current = current_branch(&repo);

    match (git.use_worktree, git.create_branch) {
        // Legacy behavior + selectable base + optional name.
        (true, true) => {
            let branch = git
                .branch_name
                .clone()
                .unwrap_or_else(|| format!("ryuzi/{short}"));
            worktree::create(
                repo_dir,
                &short,
                &branch,
                worktree_path,
                git.base_branch.as_deref(),
            )
            .map_err(|e| anyhow::anyhow!("couldn't create branch '{branch}': {e}"))?;
            Ok(PreparedWorkspace {
                work_dir: worktree_path.to_path_buf(),
                branch,
                worktree_path: Some(worktree_path.to_path_buf()),
                branch_owned: git.branch_name.is_none(),
            })
        }
        // Worktree directly on an existing branch; git's refusal (already
        // checked out elsewhere) surfaces verbatim plus a toggle hint.
        (true, false) => {
            let branch = git.base_branch.clone().unwrap_or_else(|| current.clone());
            worktree::add_for_branch(repo_dir, &short, &branch, worktree_path).map_err(|e| {
                anyhow::anyhow!(
                    "couldn't add a worktree on branch '{branch}': {e}. \
                     Pick another branch, or turn \"New branch\" on to work on a fresh branch."
                )
            })?;
            Ok(PreparedWorkspace {
                work_dir: worktree_path.to_path_buf(),
                branch,
                worktree_path: Some(worktree_path.to_path_buf()),
                branch_owned: false,
            })
        }
        // In-place new branch: refuse dirty, create from base, switch.
        (false, true) => {
            ensure_clean(&repo)?;
            let branch = git
                .branch_name
                .clone()
                .unwrap_or_else(|| format!("ryuzi/{short}"));
            let base = resolve_base_commit(&repo, git.base_branch.as_deref())?;
            repo.branch(&branch, &base, false)
                .map_err(|e| anyhow::anyhow!("couldn't create branch '{branch}': {e}"))?;
            checkout_branch(&repo, &branch)?;
            Ok(PreparedWorkspace {
                work_dir: repo_dir.to_path_buf(),
                branch,
                worktree_path: None,
                branch_owned: git.branch_name.is_none(),
            })
        }
        // In-place existing branch: same branch = touch nothing; otherwise
        // refuse dirty, then switch.
        (false, false) => {
            let branch = git.base_branch.clone().unwrap_or_else(|| current.clone());
            if branch != current {
                ensure_clean(&repo)?;
                checkout_branch(&repo, &branch)?;
            }
            Ok(PreparedWorkspace {
                work_dir: repo_dir.to_path_buf(),
                branch,
                worktree_path: None,
                branch_owned: false,
            })
        }
    }
}

/// Reject names git itself rejects, before attempting creation.
///
/// `Reference::is_valid_name("refs/heads/<name>")` alone is not enough: it
/// accepts a leading `-` (e.g. `-leading`), which `git branch`/`git_branch_create`
/// itself refuses (ambiguous with an option flag). `Branch::name_is_valid`
/// wraps libgit2's `git_branch_name_is_valid`, which applies that extra
/// branch-specific rule on top of the general ref-format checks.
pub(crate) fn validate_branch_name(name: &str) -> anyhow::Result<()> {
    let valid = !name.is_empty() && git2::Branch::name_is_valid(name).unwrap_or(false);
    if !valid {
        anyhow::bail!("'{name}' is not a valid git branch name");
    }
    Ok(())
}

/// The checked-out branch's short name; for a detached/unborn HEAD, a
/// best-effort placeholder (the caller only compares equality with real
/// branch names, which never match it).
fn current_branch(repo: &Repository) -> String {
    match repo.head() {
        Ok(head) if head.is_branch() => head.shorthand().unwrap_or("HEAD").to_string(),
        _ => "HEAD".to_string(),
    }
}

/// Dirty = staged or unstaged modifications to TRACKED files. Untracked
/// files are allowed (include_untracked(false) drops them from statuses).
fn ensure_clean(repo: &Repository) -> anyhow::Result<()> {
    let mut opts = StatusOptions::new();
    opts.include_untracked(false).include_ignored(false);
    let statuses = repo.statuses(Some(&mut opts))?;
    if !statuses.is_empty() {
        anyhow::bail!(
            "the project working tree has uncommitted changes to tracked files; \
             commit or stash them, or turn \"Worktree\" on to run in an isolated worktree"
        );
    }
    Ok(())
}

fn resolve_base_commit<'r>(
    repo: &'r Repository,
    base: Option<&str>,
) -> anyhow::Result<git2::Commit<'r>> {
    Ok(match base {
        Some(name) => repo
            .find_branch(name, BranchType::Local)?
            .get()
            .peel_to_commit()?,
        None => repo.head()?.peel_to_commit()?,
    })
}

/// In-place switch: update the working tree to the branch's tree (safe
/// checkout — refuses to clobber local changes) and move HEAD.
fn checkout_branch(repo: &Repository, branch: &str) -> anyhow::Result<()> {
    let refname = format!("refs/heads/{branch}");
    let obj = repo.revparse_single(&refname)?;
    repo.checkout_tree(&obj, None)?;
    repo.set_head(&refname)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const PK: &str = "abcdef0123456789";

    /// Init a repo with one commit containing `a.txt`; returns the default
    /// branch name (git2 may pick `master` or honor init.defaultBranch —
    /// never hardcode it).
    fn init_repo(dir: &Path) -> String {
        let repo = Repository::init(dir).unwrap();
        commit_file(&repo, "a.txt", "one", "init");
        // Bind the intermediate `Reference` (it implements `Drop`) instead of
        // chaining `.shorthand()` off a bare tail expression — the latter
        // fails borrowck (E0597) under this toolchain/git2 version.
        let head = repo.head().unwrap();
        head.shorthand().unwrap().to_string()
    }

    fn commit_file(repo: &Repository, name: &str, content: &str, msg: &str) {
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

    /// Create branch `feature` one commit ahead of `default`, then switch the
    /// checkout back to `default`. Returns feature's tip oid.
    fn add_feature_branch(repo_dir: &Path, default: &str) -> git2::Oid {
        let repo = Repository::open(repo_dir).unwrap();
        let head = repo.head().unwrap().peel_to_commit().unwrap();
        repo.branch("feature", &head, false).unwrap();
        repo.set_head("refs/heads/feature").unwrap();
        commit_file(&repo, "b.txt", "two", "feature work");
        let tip = repo.head().unwrap().peel_to_commit().unwrap().id();
        let obj = repo
            .revparse_single(&format!("refs/heads/{default}"))
            .unwrap();
        repo.checkout_tree(&obj, None).unwrap();
        repo.set_head(&format!("refs/heads/{default}")).unwrap();
        tip
    }

    fn opts(
        use_worktree: bool,
        create_branch: bool,
        branch_name: Option<&str>,
        base_branch: Option<&str>,
    ) -> SessionGitOptions {
        SessionGitOptions {
            use_worktree,
            create_branch,
            branch_name: branch_name.map(str::to_string),
            base_branch: base_branch.map(str::to_string),
        }
    }

    // ---- cell (true, true) -------------------------------------------------

    #[test]
    fn worktree_new_branch_cuts_from_base_tip_with_auto_name() {
        let repo_dir = tempfile::tempdir().unwrap();
        let default = init_repo(repo_dir.path());
        let feature_tip = add_feature_branch(repo_dir.path(), &default);
        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("abcdef01");

        let ws = prepare_session_workspace(
            repo_dir.path(),
            &opts(true, true, None, Some("feature")),
            PK,
            &wt_path,
        )
        .unwrap();

        assert_eq!(ws.branch, "ryuzi/abcdef01");
        assert!(ws.branch_owned, "auto-named branch is engine-owned");
        assert_eq!(ws.work_dir, wt_path);
        assert_eq!(ws.worktree_path.as_deref(), Some(wt_path.as_path()));
        let repo = Repository::open(repo_dir.path()).unwrap();
        let tip = repo
            .find_branch("ryuzi/abcdef01", BranchType::Local)
            .unwrap()
            .get()
            .peel_to_commit()
            .unwrap()
            .id();
        assert_eq!(tip, feature_tip, "cut from base tip, not HEAD");
    }

    #[test]
    fn worktree_new_branch_with_user_name_is_not_owned() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("abcdef01");

        let ws = prepare_session_workspace(
            repo_dir.path(),
            &opts(true, true, Some("feat/login"), None),
            PK,
            &wt_path,
        )
        .unwrap();

        assert_eq!(ws.branch, "feat/login");
        assert!(!ws.branch_owned, "user-named branch must never be deleted");
        let repo = Repository::open(repo_dir.path()).unwrap();
        assert!(repo.find_branch("feat/login", BranchType::Local).is_ok());
    }

    // ---- cell (true, false) ------------------------------------------------

    #[test]
    fn worktree_on_existing_branch_creates_no_branch() {
        let repo_dir = tempfile::tempdir().unwrap();
        let default = init_repo(repo_dir.path());
        add_feature_branch(repo_dir.path(), &default);
        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("abcdef01");

        let ws = prepare_session_workspace(
            repo_dir.path(),
            &opts(true, false, None, Some("feature")),
            PK,
            &wt_path,
        )
        .unwrap();

        assert_eq!(ws.branch, "feature");
        assert!(!ws.branch_owned);
        assert_eq!(ws.work_dir, wt_path);
        assert_eq!(ws.worktree_path.as_deref(), Some(wt_path.as_path()));
        let repo = Repository::open(repo_dir.path()).unwrap();
        let count = repo.branches(Some(BranchType::Local)).unwrap().count();
        assert_eq!(count, 2, "no branch may be created in this cell");
    }

    #[test]
    fn worktree_on_checked_out_branch_fails_with_hint() {
        let repo_dir = tempfile::tempdir().unwrap();
        let default = init_repo(repo_dir.path());
        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("abcdef01");

        let err = prepare_session_workspace(
            repo_dir.path(),
            &opts(true, false, None, Some(&default)),
            PK,
            &wt_path,
        )
        .unwrap_err();

        let msg = err.to_string();
        assert!(
            msg.contains("New branch"),
            "error must carry the toggle hint, got: {msg}"
        );
    }

    // ---- cell (false, true) ------------------------------------------------

    #[test]
    fn in_place_new_branch_checks_out_in_project_workdir() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        let wt = tempfile::tempdir().unwrap();
        let wt_path = wt.path().join("abcdef01");

        let ws = prepare_session_workspace(
            repo_dir.path(),
            &opts(false, true, None, None),
            PK,
            &wt_path,
        )
        .unwrap();

        assert_eq!(ws.work_dir, repo_dir.path());
        assert_eq!(ws.worktree_path, None);
        assert_eq!(ws.branch, "ryuzi/abcdef01");
        assert!(ws.branch_owned);
        let repo = Repository::open(repo_dir.path()).unwrap();
        assert_eq!(
            repo.head().unwrap().shorthand().unwrap(),
            "ryuzi/abcdef01",
            "the project checkout must now be on the new branch"
        );
        assert!(!wt_path.exists(), "no worktree may be created in this cell");
    }

    #[test]
    fn in_place_new_branch_refuses_dirty_tracked_files() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        // Unstaged modification to a tracked file = dirty.
        std::fs::write(repo_dir.path().join("a.txt"), "changed").unwrap();
        let wt = tempfile::tempdir().unwrap();

        let err = prepare_session_workspace(
            repo_dir.path(),
            &opts(false, true, None, None),
            PK,
            &wt.path().join("abcdef01"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("uncommitted changes"), "{err}");
    }

    #[test]
    fn in_place_new_branch_allows_untracked_files() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        // Untracked files are explicitly allowed by the dirty definition.
        std::fs::write(repo_dir.path().join("scratch.txt"), "new").unwrap();
        let wt = tempfile::tempdir().unwrap();

        prepare_session_workspace(
            repo_dir.path(),
            &opts(false, true, None, None),
            PK,
            &wt.path().join("abcdef01"),
        )
        .unwrap();
    }

    // ---- cell (false, false) -----------------------------------------------

    #[test]
    fn in_place_same_branch_touches_nothing_even_when_dirty() {
        let repo_dir = tempfile::tempdir().unwrap();
        let default = init_repo(repo_dir.path());
        std::fs::write(repo_dir.path().join("a.txt"), "changed").unwrap();
        let wt = tempfile::tempdir().unwrap();

        let ws = prepare_session_workspace(
            repo_dir.path(),
            &opts(false, false, None, Some(&default)),
            PK,
            &wt.path().join("abcdef01"),
        )
        .unwrap();

        assert_eq!(ws.branch, default);
        assert!(!ws.branch_owned);
        assert_eq!(ws.worktree_path, None);
        // The dirty edit survives untouched.
        assert_eq!(
            std::fs::read_to_string(repo_dir.path().join("a.txt")).unwrap(),
            "changed"
        );
    }

    #[test]
    fn in_place_other_branch_checks_out_when_clean() {
        let repo_dir = tempfile::tempdir().unwrap();
        let default = init_repo(repo_dir.path());
        add_feature_branch(repo_dir.path(), &default);
        let wt = tempfile::tempdir().unwrap();

        let ws = prepare_session_workspace(
            repo_dir.path(),
            &opts(false, false, None, Some("feature")),
            PK,
            &wt.path().join("abcdef01"),
        )
        .unwrap();

        assert_eq!(ws.branch, "feature");
        let repo = Repository::open(repo_dir.path()).unwrap();
        assert_eq!(repo.head().unwrap().shorthand().unwrap(), "feature");
        // feature's extra file materialized in the workdir.
        assert!(repo_dir.path().join("b.txt").exists());
    }

    #[test]
    fn in_place_other_branch_refuses_dirty() {
        let repo_dir = tempfile::tempdir().unwrap();
        let default = init_repo(repo_dir.path());
        add_feature_branch(repo_dir.path(), &default);
        std::fs::write(repo_dir.path().join("a.txt"), "changed").unwrap();
        let wt = tempfile::tempdir().unwrap();

        let err = prepare_session_workspace(
            repo_dir.path(),
            &opts(false, false, None, Some("feature")),
            PK,
            &wt.path().join("abcdef01"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("uncommitted changes"), "{err}");
    }

    // ---- validation ----------------------------------------------------------

    #[test]
    fn invalid_branch_name_is_rejected_before_any_git_mutation() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        let wt = tempfile::tempdir().unwrap();

        for bad in ["has space", "double..dot", "trailing/", "-leading", ""] {
            let err = prepare_session_workspace(
                repo_dir.path(),
                &opts(true, true, Some(bad), None),
                PK,
                &wt.path().join("abcdef01"),
            )
            .unwrap_err();
            assert!(
                err.to_string().contains("not a valid git branch name"),
                "'{bad}' should be rejected, got: {err}"
            );
        }
    }

    #[test]
    fn unknown_base_branch_is_rejected() {
        let repo_dir = tempfile::tempdir().unwrap();
        init_repo(repo_dir.path());
        let wt = tempfile::tempdir().unwrap();

        let err = prepare_session_workspace(
            repo_dir.path(),
            &opts(true, true, None, Some("nope")),
            PK,
            &wt.path().join("abcdef01"),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not exist"), "{err}");
    }
}
