//! `skill_manage` — create/patch/archive filesystem skills (Phase 4 Task 6).
//!
//! Writes land ONLY under the live skills root
//! ([`crate::skills_install::skills_root`], `~/.config/ryuzi/skills` by
//! default) — this tool bypasses the worktree jail entirely, like `memory`
//! (skills are user/agent-global, not per-project).
//!
//! Every mutating call passes through [`guard_decision`] — a pure origin ×
//! provenance × action matrix (ported from hermes-agent) — BEFORE any
//! filesystem write, so a denied call never touches disk.

use super::{jail, PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::domain::WriteOrigin;
use crate::skills_install;
use async_trait::async_trait;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

pub struct SkillManage;

/// One `skill_manage` action.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Create,
    Patch,
    Edit,
    Delete,
    WriteFile,
    RemoveFile,
}

impl Action {
    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "create" => Action::Create,
            "patch" => Action::Patch,
            "edit" => Action::Edit,
            "delete" => Action::Delete,
            "write_file" => Action::WriteFile,
            "remove_file" => Action::RemoveFile,
            _ => return None,
        })
    }

    /// Removal actions are delete-protected by a pinned skill (guard rule 2).
    fn is_removal(self) -> bool {
        matches!(self, Action::Delete | Action::RemoveFile)
    }
}

/// Where a skill came from — resolved from its `.ryuzi-skill.json` provenance
/// stamp ([`skills_install::PROVENANCE_FILE`]) plus the protected-builtin set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Provenance {
    /// No provenance stamp: created by a user or an agent directly through
    /// `skill_manage`, never installed from a pack.
    UserAuthored,
    /// Carries a `.ryuzi-skill.json` stamp: materialized by `skills_install`
    /// from a git-backed source (curated or arbitrary), possibly as part of a
    /// plugin pack.
    Installed,
    /// Reserved for any first-party skill ryuzi ships as immutable (none
    /// today) — read-only to every origin, including the user.
    ProtectedBuiltin,
}

/// Names that are always [`Provenance::ProtectedBuiltin`], regardless of
/// what's on disk. Empty today — ryuzi ships no bundled, non-editable skills
/// yet — kept as an explicit extension point so a future first-party skill
/// can opt in without touching the guard logic.
const PROTECTED_BUILTIN_SKILLS: &[&str] = &[];

/// Sub-directories `write_file`/`remove_file` may touch — never the skill's
/// own `SKILL.md` (that goes through `create`/`patch`/`edit` instead).
const ASSET_SUBDIRS: &[&str] = &["references", "templates", "scripts", "assets"];

/// The origin × provenance × action guard matrix (ported from hermes-agent
/// [04][06]). Pure so it is exhaustively unit-tested.
///
/// `viewed` is PRE-FILTERED by the caller (`execute`, below) to `{name}` when
/// the exact skill has been `skill`-viewed this turn, or left empty
/// otherwise — `name`/`ctx` are deliberately kept out of this signature so
/// the matrix stays a pure function over four small enums.
pub fn guard_decision(
    action: Action,
    prov: Provenance,
    origin: WriteOrigin,
    pinned: bool,
    viewed: &HashSet<String>,
) -> Result<(), String> {
    // 1. Protected builtins are immutable, for every origin.
    if prov == Provenance::ProtectedBuiltin {
        return Err("this is a protected built-in skill and cannot be modified".into());
    }
    // 2. Pinned skills are delete-protected (edits still allowed).
    if pinned && action.is_removal() {
        return Err("this skill is pinned (delete-protected); unpin it first".into());
    }
    // 3. Installed (bundled/hub/external) skills are read-only to autonomous
    //    origins — only an interactive user may edit what they installed.
    if prov == Provenance::Installed && origin.is_autonomous() {
        return Err(
            "installed skills are read-only to the agent; only the user may edit them".into(),
        );
    }
    // 4. The background self-review fork — the strictest origin — must have
    //    `skill`-viewed the exact skill THIS turn before mutating it, so it
    //    never edits a skill blind. Creation is exempt (nothing to view yet).
    if origin == WriteOrigin::BackgroundReview && action != Action::Create && viewed.is_empty() {
        return Err(
            "background review must `skill` (view) this exact skill before mutating it".into(),
        );
    }
    Ok(())
}

/// A skill's `.ryuzi-skill.json` provenance stamp, read just far enough to
/// resolve [`Provenance`] and (when installed as part of a pack) cross-check
/// the pack's `plugin_installs.pinned` flag. Deliberately NOT the private
/// `skills_install::SkillInstallProvenance` — this only needs `plugin_id`,
/// and duplicating a 1-field read avoids widening that struct's visibility.
#[derive(Debug, Deserialize)]
struct ProvenanceStamp {
    plugin_id: Option<String>,
}

/// Resolve `name` (SKILL.md frontmatter `name`, falling back to its
/// directory name — the same identity space as the `skill` view tool and
/// `skill_usage`) to its on-disk directory directly under `root`. Never
/// descends into `.archive/` — an archived skill is no longer addressable by
/// `skill_manage`.
fn resolve_skill_dir(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;
    for entry in entries.filter_map(std::result::Result::ok) {
        let path = entry.path();
        if !path.is_dir() {
            continue;
        }
        if path.file_name().and_then(|n| n.to_str()) == Some(".archive") {
            continue;
        }
        let Ok(text) = std::fs::read_to_string(path.join("SKILL.md")) else {
            continue;
        };
        let (frontmatter, _) = crate::harness::native::agents::split_frontmatter_pub(&text);
        let dir_name = path
            .file_name()
            .and_then(|n| n.to_str())
            .unwrap_or_default()
            .to_string();
        let mut resolved = dir_name;
        for (k, v) in frontmatter {
            if k == "name" {
                resolved = v;
            }
        }
        if resolved == name {
            return Some(path);
        }
    }
    None
}

/// Resolve a skill's [`Provenance`] and effective pinned state: pinned is
/// `skill_usage.pinned` OR'd with its installed pack's `plugin_installs.pinned`
/// (an individual skill pin protects it directly; a pack pin protects every
/// skill materialized from that pack).
async fn provenance_and_pin(ctx: &ToolCtx, dir: &Path, name: &str) -> (Provenance, bool) {
    if PROTECTED_BUILTIN_SKILLS.contains(&name) {
        return (Provenance::ProtectedBuiltin, false);
    }
    let stamp_path = dir.join(skills_install::PROVENANCE_FILE);
    let (provenance, plugin_id) = match std::fs::read_to_string(&stamp_path) {
        Ok(text) => {
            let plugin_id = serde_json::from_str::<ProvenanceStamp>(&text)
                .ok()
                .and_then(|s| s.plugin_id);
            (Provenance::Installed, plugin_id)
        }
        Err(_) => (Provenance::UserAuthored, None),
    };
    let mut pinned = ctx
        .store
        .get_skill_usage(name)
        .await
        .ok()
        .flatten()
        .map(|u| u.pinned)
        .unwrap_or(false);
    if let Some(pid) = plugin_id {
        if let Ok(Some(rec)) = ctx.store.get_plugin_install(&pid).await {
            pinned = pinned || rec.pinned;
        }
    }
    (provenance, pinned)
}

/// A path-safe slug: no separators, no leading dot (so a skill can never be
/// named `.archive` or a hidden/traversal-looking segment).
fn valid_skill_name(name: &str) -> bool {
    !name.is_empty()
        && !name.starts_with('.')
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
}

/// Render a `SKILL.md` — the same `---\nkey: value\n---\nbody` shape
/// `skills::parse_skill` expects.
fn render_skill_md(name: &str, description: &str, body: &str) -> String {
    format!(
        "---\nname: {name}\ndescription: {description}\n---\n{}\n",
        body.trim()
    )
}

/// Read an existing `SKILL.md`'s frontmatter, falling back to the directory
/// name / an empty description when a field is missing (defensive — every
/// skill `skill_manage` itself created has both, but a hand-authored one
/// might not).
fn read_skill_md(dir: &Path) -> Result<(String, String, String), String> {
    let path = dir.join("SKILL.md");
    let text = std::fs::read_to_string(&path).map_err(|e| format!("cannot read SKILL.md: {e}"))?;
    let (frontmatter, body) = crate::harness::native::agents::split_frontmatter_pub(&text);
    let mut name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("skill")
        .to_string();
    let mut description = String::new();
    for (k, v) in frontmatter {
        match k.as_str() {
            "name" => name = v,
            "description" => description = v,
            _ => {}
        }
    }
    Ok((name, description, body))
}

fn create_skill(root: &Path, name: &str, input: &Value) -> Result<String, String> {
    let Some(description) = input
        .get("description")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    else {
        return Err("`description` is required for create".into());
    };
    let body = input.get("body").and_then(|v| v.as_str()).unwrap_or("");
    let dir = root.join(name);
    if dir.exists() {
        return Err(format!("a skill named `{name}` already exists"));
    }
    std::fs::create_dir_all(&dir).map_err(|e| format!("cannot create skill directory: {e}"))?;
    std::fs::write(
        dir.join("SKILL.md"),
        render_skill_md(name, description, body),
    )
    .map_err(|e| format!("cannot write SKILL.md: {e}"))?;
    Ok(format!("created skill `{name}`"))
}

fn patch_skill(dir: &Path, input: &Value) -> Result<String, String> {
    let Some(text) = input
        .get("text")
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
    else {
        return Err("`text` is required for patch".into());
    };
    let (name, description, body) = read_skill_md(dir)?;
    let new_body = format!("{}\n\n{}", body.trim_end(), text.trim());
    std::fs::write(
        dir.join("SKILL.md"),
        render_skill_md(&name, &description, &new_body),
    )
    .map_err(|e| format!("cannot write SKILL.md: {e}"))?;
    Ok(format!("patched `{name}`"))
}

fn edit_skill(dir: &Path, input: &Value) -> Result<String, String> {
    let new_description = input.get("description").and_then(|v| v.as_str());
    let new_body = input.get("body").and_then(|v| v.as_str());
    if new_description.is_none() && new_body.is_none() {
        return Err("edit requires `description` and/or `body`".into());
    }
    let (name, description, body) = read_skill_md(dir)?;
    let description = new_description.unwrap_or(&description).to_string();
    let body = new_body.unwrap_or(&body).to_string();
    std::fs::write(
        dir.join("SKILL.md"),
        render_skill_md(&name, &description, &body),
    )
    .map_err(|e| format!("cannot write SKILL.md: {e}"))?;
    Ok(format!("edited `{name}`"))
}

/// Whole-skill removal. An autonomous origin never hard-deletes: it archives
/// (moves the dir under `<root>/.archive/`, marks `skill_usage.state =
/// "archived"`) and MUST justify the removal with `absorbed_into` — fail
/// closed with no filesystem mutation when that argument is missing. A human
/// (`WriteOrigin::User`) may hard-delete directly.
async fn delete_skill(
    ctx: &ToolCtx,
    root: &Path,
    dir: &Path,
    name: &str,
    input: &Value,
) -> Result<String, String> {
    let now = crate::paths::now_ms();
    if ctx.write_origin.is_autonomous() {
        let Some(absorbed_into) = input
            .get("absorbed_into")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
        else {
            return Err(
                "an autonomous caller must archive, not hard-delete — pass `absorbed_into` \
                 naming where this skill's content went"
                    .into(),
            );
        };
        let archive_root = root.join(".archive");
        std::fs::create_dir_all(&archive_root)
            .map_err(|e| format!("cannot prepare the archive: {e}"))?;
        let archive_dir = archive_root.join(format!("{name}-{now}"));
        std::fs::rename(dir, &archive_dir).map_err(|e| format!("cannot archive skill: {e}"))?;
        let _ = std::fs::write(
            archive_dir.join("ABSORBED_INTO.md"),
            format!("Archived by an autonomous review. Absorbed into: {absorbed_into}\n"),
        );
        ctx.store
            .set_skill_state(name, "archived", Some(now))
            .await
            .map_err(|e| e.to_string())?;
        Ok(format!("archived `{name}` (absorbed into {absorbed_into})"))
    } else {
        std::fs::remove_dir_all(dir).map_err(|e| format!("cannot delete skill: {e}"))?;
        ctx.store
            .set_skill_state(name, "deleted", Some(now))
            .await
            .map_err(|e| e.to_string())?;
        Ok(format!("deleted `{name}`"))
    }
}

/// Resolve `rel` to a path under `skill_dir`, restricted to the four asset
/// subdirectories and jailed against traversal/absolute-path/symlink escape
/// (reuses [`jail`]'s already-tested `sandbox` machinery, rooted at the
/// skill's own directory rather than the worktree).
fn asset_path(skill_dir: &Path, rel: &str) -> Result<PathBuf, String> {
    let first_ok = Path::new(rel)
        .components()
        .next()
        .and_then(|c| c.as_os_str().to_str())
        .map(|s| ASSET_SUBDIRS.contains(&s))
        .unwrap_or(false);
    if !first_ok {
        return Err(format!(
            "file path must start with one of: {}/",
            ASSET_SUBDIRS.join("/, ")
        ));
    }
    jail(skill_dir, rel).map_err(|e| e.to_string())
}

fn write_asset_file(dir: &Path, input: &Value) -> Result<String, String> {
    let Some(rel) = input.get("path").and_then(|v| v.as_str()) else {
        return Err("`path` is required for write_file".into());
    };
    let content = input.get("content").and_then(|v| v.as_str()).unwrap_or("");
    let target = asset_path(dir, rel)?;
    if let Some(parent) = target.parent() {
        std::fs::create_dir_all(parent).map_err(|e| format!("cannot create parent dirs: {e}"))?;
    }
    std::fs::write(&target, content).map_err(|e| format!("cannot write {rel}: {e}"))?;
    Ok(format!("wrote {rel}"))
}

fn remove_asset_file(dir: &Path, input: &Value) -> Result<String, String> {
    let Some(rel) = input.get("path").and_then(|v| v.as_str()) else {
        return Err("`path` is required for remove_file".into());
    };
    let target = asset_path(dir, rel)?;
    if !target.is_file() {
        return Err(format!("no such file `{rel}`"));
    }
    std::fs::remove_file(&target).map_err(|e| format!("cannot remove {rel}: {e}"))?;
    Ok(format!("removed {rel}"))
}

#[async_trait]
impl Tool for SkillManage {
    fn name(&self) -> &str {
        "skill_manage"
    }
    fn description(&self) -> &str {
        "Create, patch, edit, or archive filesystem skills under the global \
         skills root — author new capabilities for yourself and future \
         turns. Actions: `create` {name, description, body} a brand-new \
         skill; `patch` {name, text} append a short amendment to an \
         existing skill's body; `edit` {name, description?, body?} replace \
         an existing skill's content; `delete` {name, absorbed_into?} \
         remove a skill (an autonomous caller archives rather than \
         hard-deletes, and must pass `absorbed_into`); `write_file`/ \
         `remove_file` {name, path, content?} manage files under the \
         skill's references/templates/scripts/assets subdirectories. \
         Installed and pinned skills are read-only to autonomous callers; \
         `skill`-view a skill before patching it."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["create", "patch", "edit", "delete", "write_file", "remove_file"]
                },
                "name": {"type": "string", "description": "Skill name (its SKILL.md frontmatter `name`)."},
                "description": {"type": "string", "description": "create/edit: SKILL.md frontmatter description (~60 chars, shown in the index)."},
                "body": {"type": "string", "description": "create/edit: the skill's full markdown body."},
                "text": {"type": "string", "description": "patch: text appended to the skill's existing body."},
                "path": {"type": "string", "description": "write_file/remove_file: path relative to the skill dir, under references/, templates/, scripts/, or assets/."},
                "content": {"type": "string", "description": "write_file: file content."},
                "absorbed_into": {"type": "string", "description": "delete: required for an autonomous caller — where this skill's content went."}
            },
            "required": ["action", "name"]
        })
    }
    fn kind(&self) -> &'static str {
        // Bypasses the worktree jail/snapshot entirely, like `memory` — this
        // tool never touches the session worktree.
        "other"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let name = input.get("name").and_then(|v| v.as_str()).unwrap_or("");
        let action = input.get("action").and_then(|v| v.as_str()).unwrap_or("?");
        PermissionSpec::new("skill_manage", format!("{action} skill {name}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let Some(action_str) = input.get("action").and_then(|v| v.as_str()) else {
            return Ok(ToolOutput::error(
                "skill_manage: `action` is required (create|patch|edit|delete|write_file|remove_file)",
            ));
        };
        let Some(action) = Action::parse(action_str) else {
            return Ok(ToolOutput::error(format!(
                "skill_manage: unknown action `{action_str}`"
            )));
        };
        let Some(name) = input
            .get("name")
            .and_then(|v| v.as_str())
            .filter(|s| !s.trim().is_empty())
        else {
            return Ok(ToolOutput::error("skill_manage: `name` is required"));
        };
        if !valid_skill_name(name) {
            return Ok(ToolOutput::error(
                "skill_manage: `name` must be a simple slug (letters, digits, `-`, `_`) — no path separators",
            ));
        }

        let root = match skills_install::skills_root() {
            Ok(r) => r,
            Err(e) => return Ok(ToolOutput::error(format!("skill_manage: {e}"))),
        };
        if let Err(e) = std::fs::create_dir_all(&root) {
            return Ok(ToolOutput::error(format!(
                "skill_manage: cannot prepare the skills root: {e}"
            )));
        }

        // The guard runs BEFORE any filesystem write, for every action —
        // including `create`, where it is provably a no-op (fresh,
        // unpinned, `UserAuthored`, and exempt from the viewed check) but is
        // still invoked so that invariant holds uniformly and visibly.
        let outcome: Result<String, String> = if action == Action::Create {
            match guard_decision(
                Action::Create,
                Provenance::UserAuthored,
                ctx.write_origin,
                false,
                &HashSet::new(),
            ) {
                Err(msg) => Err(msg),
                Ok(()) => create_skill(&root, name, &input),
            }
        } else {
            match resolve_skill_dir(&root, name) {
                None => {
                    return Ok(ToolOutput::error(format!(
                        "skill_manage: no skill named `{name}` under the skills root"
                    )))
                }
                Some(dir) => {
                    let (provenance, pinned) = provenance_and_pin(ctx, &dir, name).await;
                    let viewed = {
                        let seen = ctx.viewed_skills.lock().await;
                        if seen.contains(name) {
                            HashSet::from([name.to_string()])
                        } else {
                            HashSet::new()
                        }
                    };
                    match guard_decision(action, provenance, ctx.write_origin, pinned, &viewed) {
                        Err(msg) => Err(msg),
                        Ok(()) => match action {
                            Action::Create => unreachable!("handled above"),
                            Action::Patch => patch_skill(&dir, &input),
                            Action::Edit => edit_skill(&dir, &input),
                            Action::Delete => delete_skill(ctx, &root, &dir, name, &input).await,
                            Action::WriteFile => write_asset_file(&dir, &input),
                            Action::RemoveFile => remove_asset_file(&dir, &input),
                        },
                    }
                }
            }
        };

        match outcome {
            Ok(msg) => {
                if action == Action::Create {
                    if ctx.write_origin.is_autonomous() {
                        let _ = ctx.store.mark_skill_created_by_agent(name).await;
                    }
                } else {
                    let _ = ctx.store.record_skill_patch(name).await;
                }
                Ok(ToolOutput::ok(format!("skill_manage: {msg}")))
            }
            Err(msg) => Ok(ToolOutput::error(format!("skill_manage: {msg}"))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn viewed(names: &[&str]) -> HashSet<String> {
        names.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn guard_matrix_enforces_provenance_and_origin() {
        // A user-authored, unpinned skill: any origin may edit/delete.
        assert!(guard_decision(
            Action::Delete,
            Provenance::UserAuthored,
            WriteOrigin::User,
            false,
            &viewed(&[])
        )
        .is_ok());
        // Pinned: delete-protected for everyone; edits allowed.
        assert!(guard_decision(
            Action::Delete,
            Provenance::UserAuthored,
            WriteOrigin::User,
            true,
            &viewed(&[])
        )
        .is_err());
        assert!(guard_decision(
            Action::Edit,
            Provenance::UserAuthored,
            WriteOrigin::User,
            true,
            &viewed(&["s"])
        )
        .is_ok());
        // Installed (bundled/hub/external) skills are read-only to autonomous origins.
        assert!(guard_decision(
            Action::Edit,
            Provenance::Installed,
            WriteOrigin::Agent,
            false,
            &viewed(&["s"])
        )
        .is_err());
        assert!(guard_decision(
            Action::Edit,
            Provenance::Installed,
            WriteOrigin::User,
            false,
            &viewed(&["s"])
        )
        .is_ok());
        // A review fork must have skill_view'd the exact skill before mutating it.
        assert!(guard_decision(
            Action::Patch,
            Provenance::UserAuthored,
            WriteOrigin::BackgroundReview,
            false,
            &viewed(&[])
        )
        .is_err());
        assert!(guard_decision(
            Action::Patch,
            Provenance::UserAuthored,
            WriteOrigin::BackgroundReview,
            false,
            &viewed(&["s"])
        )
        .is_ok());
        // Protected builtins: read-only always.
        assert!(guard_decision(
            Action::Edit,
            Provenance::ProtectedBuiltin,
            WriteOrigin::User,
            false,
            &viewed(&["s"])
        )
        .is_err());
    }

    // ── execute()-level tests ──────────────────────────────────────────
    //
    // `skills_install::skills_root()` resolves via `InstallRoots::for_user()`,
    // which honors `RYUZI_TEST_CONFIG_ROOT` under `#[cfg(test)]`. That env var
    // is process-global, so every test using `ConfigRootGuard` is `#[serial]`.

    use super::super::testutil::ctx_at;
    use serial_test::serial;

    struct ConfigRootGuard {
        _dir: tempfile::TempDir,
    }

    impl ConfigRootGuard {
        /// Points `skills_install::skills_root()` at a fresh tempdir; returns
        /// the guard (keep it alive for the test's duration) and the
        /// resolved skills-root path.
        fn new() -> (Self, PathBuf) {
            let dir = tempfile::tempdir().unwrap();
            std::env::set_var("RYUZI_TEST_CONFIG_ROOT", dir.path());
            let root = dir.path().join("skills");
            (ConfigRootGuard { _dir: dir }, root)
        }
    }

    impl Drop for ConfigRootGuard {
        fn drop(&mut self) {
            std::env::remove_var("RYUZI_TEST_CONFIG_ROOT");
        }
    }

    async fn ctx_with_origin(dir: &Path, origin: WriteOrigin) -> super::super::ToolCtx {
        let mut ctx = ctx_at(dir).await;
        ctx.write_origin = origin;
        ctx
    }

    /// Write a skill directly to disk (bypassing the tool), optionally
    /// stamping it as installed (`plugin_id: None` for a bare single-skill
    /// install, `Some(id)` for a pack-materialized one).
    fn seed_skill(
        root: &Path,
        name: &str,
        body: &str,
        installed_plugin_id: Option<&str>,
    ) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: d\n---\n{body}"),
        )
        .unwrap();
        if let Some(pid) = installed_plugin_id {
            std::fs::write(
                dir.join(skills_install::PROVENANCE_FILE),
                format!(
                    r#"{{"source":"https://github.com/x/y","plugin_id":"{pid}","installed_at":"now"}}"#
                ),
            )
            .unwrap();
        }
        dir
    }

    #[tokio::test]
    #[serial]
    async fn tool_is_registered() {
        let reg = super::super::ToolRegistry::builtin();
        assert!(reg.get("skill_manage").is_some());
    }

    #[tokio::test]
    #[serial]
    async fn create_writes_skill_md_and_marks_agent_created_for_autonomous_origin() {
        let (_guard, root) = ConfigRootGuard::new();
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::Agent).await;
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "create", "name": "deploy", "description": "How to deploy", "body": "Run make deploy."}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        let md = std::fs::read_to_string(root.join("deploy/SKILL.md")).unwrap();
        assert!(md.contains("Run make deploy."), "{md}");
        let usage = ctx.store.get_skill_usage("deploy").await.unwrap().unwrap();
        assert_eq!(usage.created_by.as_deref(), Some("agent"));
    }

    #[tokio::test]
    #[serial]
    async fn create_for_a_user_origin_does_not_mark_agent_created() {
        let (_guard, _root) = ConfigRootGuard::new();
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::User).await;
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "create", "name": "deploy", "description": "How to deploy", "body": "x"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        let usage = ctx.store.get_skill_usage("deploy").await.unwrap();
        assert!(usage.is_none() || usage.unwrap().created_by.is_none());
    }

    #[tokio::test]
    #[serial]
    async fn create_twice_is_a_clean_error_and_does_not_clobber() {
        let (_guard, root) = ConfigRootGuard::new();
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::User).await;
        let first = SkillManage
            .execute(
                &ctx,
                json!({"action": "create", "name": "deploy", "description": "d", "body": "first"}),
            )
            .await
            .unwrap();
        assert!(!first.is_error, "{}", first.for_model);
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "create", "name": "deploy", "description": "d", "body": "second"}),
            )
            .await
            .unwrap();
        // The second create() call above already errored; content survives.
        let md = std::fs::read_to_string(root.join("deploy/SKILL.md")).unwrap();
        assert!(md.contains("first"), "{md}");
        assert!(!md.contains("second"), "{md}");
        assert!(out.is_error);
    }

    #[tokio::test]
    #[serial]
    async fn patch_appends_to_body_and_bumps_patch_count() {
        let (_guard, root) = ConfigRootGuard::new();
        seed_skill(&root, "deploy", "Original body.", None);
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::User).await;
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "patch", "name": "deploy", "text": "Learned: use --yes."}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        let md = std::fs::read_to_string(root.join("deploy/SKILL.md")).unwrap();
        assert!(md.contains("Original body."), "{md}");
        assert!(md.contains("Learned: use --yes."), "{md}");
        let usage = ctx.store.get_skill_usage("deploy").await.unwrap().unwrap();
        assert_eq!(usage.patch_count, 1);
    }

    #[tokio::test]
    #[serial]
    async fn autonomous_origin_cannot_edit_an_installed_skill_and_file_is_unchanged() {
        // The load-bearing test: a denied write must NOT touch the file.
        let (_guard, root) = ConfigRootGuard::new();
        seed_skill(&root, "pdf", "Original body.", Some("acme-pack"));
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::Agent).await;
        // Viewed, so the outcome is attributable to the Installed+autonomous
        // rule specifically, not the (separate) BackgroundReview-viewed gate.
        ctx.viewed_skills.lock().await.insert("pdf".to_string());
        let before = std::fs::read_to_string(root.join("pdf/SKILL.md")).unwrap();

        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "edit", "name": "pdf", "body": "HACKED"}),
            )
            .await
            .unwrap();

        assert!(out.is_error, "{}", out.for_model);
        assert!(out.for_model.contains("read-only"), "{}", out.for_model);
        let after = std::fs::read_to_string(root.join("pdf/SKILL.md")).unwrap();
        assert_eq!(before, after, "a denied write must not touch the file");
        // No patch bookkeeping either — a denied call is a full no-op.
        let usage = ctx.store.get_skill_usage("pdf").await.unwrap();
        assert!(usage.is_none() || usage.unwrap().patch_count == 0);
    }

    #[tokio::test]
    #[serial]
    async fn user_origin_may_edit_an_installed_skill() {
        let (_guard, root) = ConfigRootGuard::new();
        seed_skill(&root, "pdf", "Original body.", Some("acme-pack"));
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::User).await;
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "edit", "name": "pdf", "body": "Updated body."}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        let after = std::fs::read_to_string(root.join("pdf/SKILL.md")).unwrap();
        assert!(after.contains("Updated body."), "{after}");
    }

    #[tokio::test]
    #[serial]
    async fn background_review_must_view_before_patching_and_file_is_unchanged_until_then() {
        let (_guard, root) = ConfigRootGuard::new();
        seed_skill(&root, "deploy", "Original body.", None);
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::BackgroundReview).await;
        let before = std::fs::read_to_string(root.join("deploy/SKILL.md")).unwrap();

        // Not viewed yet: denied, file untouched.
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "patch", "name": "deploy", "text": "note"}),
            )
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.for_model);
        assert_eq!(
            before,
            std::fs::read_to_string(root.join("deploy/SKILL.md")).unwrap()
        );

        // View it, then the same patch succeeds.
        ctx.viewed_skills.lock().await.insert("deploy".to_string());
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "patch", "name": "deploy", "text": "note"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        let after = std::fs::read_to_string(root.join("deploy/SKILL.md")).unwrap();
        assert!(after.contains("note"), "{after}");
    }

    #[tokio::test]
    #[serial]
    async fn pinned_skill_blocks_delete_but_allows_patch() {
        let (_guard, root) = ConfigRootGuard::new();
        seed_skill(&root, "deploy", "Original body.", None);
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::User).await;
        ctx.store.set_skill_pinned("deploy", true).await.unwrap();

        let out = SkillManage
            .execute(&ctx, json!({"action": "delete", "name": "deploy"}))
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.for_model);
        assert!(root.join("deploy").exists());

        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "patch", "name": "deploy", "text": "still editable"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
    }

    #[tokio::test]
    #[serial]
    async fn pack_level_pin_in_plugin_installs_protects_a_materialized_skill_from_delete() {
        let (_guard, root) = ConfigRootGuard::new();
        seed_skill(&root, "acme-pack--pdf", "Original body.", Some("acme-pack"));
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::User).await;
        let now = crate::paths::now_ms();
        ctx.store
            .upsert_plugin_install(&crate::store::PluginInstallRecord {
                plugin_id: "acme-pack".into(),
                kind: "plugin_pack".into(),
                source_spec: "https://github.com/x/y".into(),
                resolved_commit: None,
                fingerprint: "sha256:x".into(),
                installed_at: now,
                updated_at: now,
                pinned: true,
                pin_reason: Some("protect it".into()),
                trust_tier: "acknowledged".into(),
                trust_ack_at: Some(now),
                trust_ack_summary: None,
            })
            .await
            .unwrap();

        let out = SkillManage
            .execute(&ctx, json!({"action": "delete", "name": "acme-pack--pdf"}))
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.for_model);
        assert!(root.join("acme-pack--pdf").exists());
    }

    #[tokio::test]
    #[serial]
    async fn autonomous_delete_requires_absorbed_into_and_archives_instead_of_hard_deleting() {
        let (_guard, root) = ConfigRootGuard::new();
        seed_skill(&root, "deploy", "Original body.", None);
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::Agent).await;

        // Missing `absorbed_into`: fail closed, nothing touched.
        let out = SkillManage
            .execute(&ctx, json!({"action": "delete", "name": "deploy"}))
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.for_model);
        assert!(root.join("deploy").exists());

        // With it: archived (moved under .archive/), never hard-deleted.
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "delete", "name": "deploy", "absorbed_into": "deploy-v2"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(!root.join("deploy").exists());
        let archived: Vec<_> = std::fs::read_dir(root.join(".archive"))
            .unwrap()
            .filter_map(|e| e.ok())
            .collect();
        assert_eq!(archived.len(), 1);
        let usage = ctx.store.get_skill_usage("deploy").await.unwrap().unwrap();
        assert_eq!(usage.state, "archived");
    }

    #[tokio::test]
    #[serial]
    async fn user_delete_is_a_hard_delete() {
        let (_guard, root) = ConfigRootGuard::new();
        seed_skill(&root, "deploy", "Original body.", None);
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::User).await;
        let out = SkillManage
            .execute(&ctx, json!({"action": "delete", "name": "deploy"}))
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(!root.join("deploy").exists());
        assert!(!root.join(".archive").exists());
    }

    #[tokio::test]
    #[serial]
    async fn write_file_and_remove_file_are_restricted_to_asset_subdirs() {
        let (_guard, root) = ConfigRootGuard::new();
        seed_skill(&root, "deploy", "Original body.", None);
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::User).await;

        // Rejected: not under an asset subdir.
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "write_file", "name": "deploy", "path": "SKILL.md", "content": "HACKED"}),
            )
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.for_model);
        let md = std::fs::read_to_string(root.join("deploy/SKILL.md")).unwrap();
        assert!(!md.contains("HACKED"), "{md}");

        // Rejected: traversal escape.
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "write_file", "name": "deploy", "path": "references/../../escape.txt", "content": "x"}),
            )
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.for_model);
        assert!(!root.join("escape.txt").exists());

        // Allowed: under references/.
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "write_file", "name": "deploy", "path": "references/notes.md", "content": "hello"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert_eq!(
            std::fs::read_to_string(root.join("deploy/references/notes.md")).unwrap(),
            "hello"
        );

        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "remove_file", "name": "deploy", "path": "references/notes.md"}),
            )
            .await
            .unwrap();
        assert!(!out.is_error, "{}", out.for_model);
        assert!(!root.join("deploy/references/notes.md").exists());
    }

    #[tokio::test]
    #[serial]
    async fn unknown_skill_is_a_clean_error() {
        let (_guard, _root) = ConfigRootGuard::new();
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::User).await;
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "patch", "name": "nope", "text": "x"}),
            )
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.for_model);
        assert!(
            out.for_model.contains("no skill named"),
            "{}",
            out.for_model
        );
    }

    #[tokio::test]
    #[serial]
    async fn invalid_skill_name_is_rejected_before_touching_disk() {
        let (_guard, root) = ConfigRootGuard::new();
        let wd = tempfile::tempdir().unwrap();
        let ctx = ctx_with_origin(wd.path(), WriteOrigin::User).await;
        let out = SkillManage
            .execute(
                &ctx,
                json!({"action": "create", "name": "../escape", "description": "d", "body": "x"}),
            )
            .await
            .unwrap();
        assert!(out.is_error, "{}", out.for_model);
        assert!(!root.parent().unwrap().join("escape").exists());
    }
}
