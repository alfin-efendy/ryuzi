//! Project provisioning: connecting an existing repo as a project, and the
//! gateway-driven create-or-clone flow with its settings/permission gating.

use super::{basename_of, ControlPlane};
use crate::domain::{PermMode, Project};
use crate::paths::{new_id, now_ms};
use crate::policy::{gate_perm_mode, is_admin, parse_role_ids};
use crate::settings::{expand_home, SettingsStore};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Per-request overrides for a provisioned project — `None` means "fall back
/// to the admin-configured default setting" (see `provision_project`).
#[derive(Debug, Clone, Default)]
pub struct ProvisionSettings {
    pub model: Option<String>,
    pub effort: Option<String>,
    pub perm_mode: Option<PermMode>,
}

/// Request to provision (create-from-name or clone-from-`git_url`) a project
/// and bind it to the gateway workspace that triggered it (the Discord
/// `/connect` flow).
#[derive(Debug, Clone)]
pub struct ProvisionProjectRequest {
    pub gateway: String,
    pub workspace_id: String,
    pub actor: String,
    pub actor_role_ids: Vec<String>,
    pub name: Option<String>,
    pub git_url: Option<String>,
    pub settings: ProvisionSettings,
}

/// Rejects `.`, `..`, any leading-dot name, and anything outside
/// `[A-Za-z0-9._-]+`.
fn validate_project_name(name: &str) -> anyhow::Result<()> {
    let ok = name != "."
        && name != ".."
        && !name.starts_with('.')
        && !name.is_empty()
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'));
    if !ok {
        anyhow::bail!("invalid project name: {name}");
    }
    Ok(())
}

/// Strips AT MOST one trailing `/`, unlike `str::trim_end_matches` which
/// would strip all of them.
fn strip_one_trailing_slash(s: &str) -> &str {
    s.strip_suffix('/').unwrap_or(s)
}

/// Run `git` with `args`, failing with the captured stderr on a non-zero
/// exit.
async fn run_git(args: &[&str]) -> anyhow::Result<()> {
    let mut cmd = tokio::process::Command::new("git");
    cmd.args(args);
    crate::process_util::no_window(&mut cmd);
    let output = cmd.output().await?;
    if !output.status.success() {
        anyhow::bail!(
            "git {} failed: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(())
}

impl ControlPlane {
    /// Connect an existing local folder (git repo or not) as a project.
    /// Non-git folders are allowed — git features (branches, worktrees,
    /// review diffs) are disabled for them in the UI, and the flag
    /// self-corrects after a later `git init`.
    pub async fn connect_project(&self, workdir: &Path, name: &str) -> anyhow::Result<Project> {
        let is_git = git2::Repository::open(workdir).is_ok();
        let project = Project {
            project_id: new_id(),
            name: name.to_string(),
            workdir: workdir.to_string_lossy().into_owned(),
            source: None,
            harness: "native".to_string(),
            model: None,
            effort: None,
            perm_mode: PermMode::Default,
            created_at: Some(now_ms()),
            is_git,
        };
        self.store.insert_project(project.clone()).await?;
        Ok(project)
    }

    /// Clone `url` into `<dest_parent>/<repo-name>` and register it as a
    /// project on the default `native` harness — the Cockpit "New project →
    /// Clone from URL" flow. Unlike [`provision_project`] this is
    /// gateway-free: no `workdir_root` setting, no admin gating, no
    /// workspace binding.
    pub async fn clone_project(&self, url: &str, dest_parent: &Path) -> anyhow::Result<Project> {
        // Strip a trailing `.git` and extract the directory name.
        let url_path = url.strip_suffix(".git").unwrap_or(url);
        let mut name = basename_of(url_path);
        if name.is_empty() {
            // Fallback: if basename is empty, use the parent directory name.
            name = basename_of(strip_one_trailing_slash(url_path));
        }
        validate_project_name(&name)?;
        let workdir = dest_parent.join(&name);
        // Refuse to clone over anything that exists — the rollback below
        // removes `workdir`, which must never delete user data.
        if workdir.exists() {
            anyhow::bail!("destination already exists: {}", workdir.display());
        }
        tokio::fs::create_dir_all(dest_parent).await?;
        let wd = workdir.to_string_lossy().into_owned();
        // `--` separates options from positionals so an untrusted `url`
        // can never be parsed by git as a flag (see `provision_project`).
        if let Err(e) = run_git(&["clone", "--quiet", "--", url, &wd]).await {
            let _ = tokio::fs::remove_dir_all(&workdir).await;
            return Err(e);
        }
        let project = Project {
            project_id: new_id(),
            name,
            workdir: wd,
            source: Some(url.to_string()),
            harness: "native".to_string(),
            model: None,
            effort: None,
            perm_mode: PermMode::Default,
            created_at: Some(now_ms()),
            is_git: true,
        };
        self.store.insert_project(project.clone()).await?;
        Ok(project)
    }

    /// Discord-driven (or any gateway's) project provisioning: create a
    /// brand-new git repo under `workdir_root`, or clone an existing one,
    /// then bind it to the gateway workspace that triggered it.
    ///
    /// Deliberately not recorded: who provisioned the project. The
    /// `projects` table has no `created_by` column — `Session.started_by`
    /// already covers per-turn auditability.
    pub async fn provision_project(&self, req: ProvisionProjectRequest) -> anyhow::Result<Project> {
        let settings = SettingsStore::new(Arc::clone(&self.store));
        let raw_root = settings
            .get("workdir_root")
            .await?
            .filter(|s| !s.is_empty())
            .ok_or_else(|| anyhow::anyhow!("workdir_root is not set"))?;
        let root = expand_home(&raw_root);

        let name: String;
        let mut source: Option<String> = None;
        let workdir: PathBuf;

        if let Some(n) = &req.name {
            validate_project_name(n)?;
            name = n.clone();
            workdir = root.join(&name);
            tokio::fs::create_dir_all(&workdir).await?;
            let wd = workdir.to_string_lossy().into_owned();
            let result: anyhow::Result<()> = async {
                run_git(&["-C", &wd, "init", "-q"]).await?;
                run_git(&["-C", &wd, "commit", "-q", "--allow-empty", "-m", "init"]).await?;
                Ok(())
            }
            .await;
            if let Err(e) = result {
                let _ = tokio::fs::remove_dir_all(&workdir).await;
                return Err(e);
            }
        } else if let Some(url) = &req.git_url {
            // Strip a trailing `.git` and extract the directory name.
            let url_path = url.strip_suffix(".git").unwrap_or(url);
            let mut n = basename_of(url_path);
            if n.is_empty() {
                // Fallback: if basename is empty, use the parent directory name.
                n = basename_of(strip_one_trailing_slash(url_path));
            }
            validate_project_name(&n)?;
            name = n;
            workdir = root.join(&name);
            let wd = workdir.to_string_lossy().into_owned();
            // `--` separates options from positionals so an untrusted
            // `url` (e.g. one starting with `-`, like `--upload-pack=evil`)
            // can never be parsed by git as a flag. Not shell injection —
            // `run_git` uses `tokio::process::Command` directly, no shell —
            // but a git-CLI option-injection hardening measure.
            if let Err(e) = run_git(&["clone", "--quiet", "--", url, &wd]).await {
                let _ = tokio::fs::remove_dir_all(&workdir).await;
                return Err(e);
            }
            source = Some(url.clone());
        } else {
            anyhow::bail!("connectProject requires name or gitUrl");
        }

        let s = &req.settings;
        let default_perm_raw = settings
            .get("default_perm_mode")
            .await?
            .unwrap_or_else(|| "default".to_string());
        let requested_mode = s
            .perm_mode
            .unwrap_or_else(|| PermMode::from_db(&default_perm_raw));
        let admin_role_ids = parse_role_ids(settings.get("admin_role_ids").await?.as_deref());
        let admin = is_admin(&req.actor_role_ids, &admin_role_ids);
        let (perm_mode, _downgraded) = gate_perm_mode(requested_mode, admin);

        let default_model = settings
            .get("default_model")
            .await?
            .filter(|v| !v.is_empty());
        let default_effort = settings
            .get("default_effort")
            .await?
            .filter(|v| !v.is_empty());

        let project = Project {
            project_id: new_id(),
            name,
            workdir: workdir.to_string_lossy().into_owned(),
            source,
            harness: "native".to_string(),
            model: s.model.clone().or(default_model),
            effort: s.effort.clone().or(default_effort),
            perm_mode,
            created_at: Some(now_ms()),
            is_git: true,
        };
        self.store.insert_project(project.clone()).await?;
        self.store
            .bind_project(&req.gateway, &req.workspace_id, &project.project_id)
            .await?;
        Ok(project)
    }
}
