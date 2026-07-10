//! Installer for git-backed native skills and plugin-bundled skill packs.

use anyhow::{anyhow, bail, Context, Result};
use chrono::{SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

pub(crate) const PROVENANCE_FILE: &str = ".ryuzi-skill.json";
const CURATED_SKILL_SOURCES: &[(&str, &str)] = &[
    ("superpowers", "https://github.com/obra/superpowers"),
    ("obra/superpowers", "https://github.com/obra/superpowers"),
];

/// A curated skill pack the Cockpit catalog offers before it's installed.
/// Distinct from `CURATED_SKILL_SOURCES`, which is an alias table for
/// `parse_skill_source` (several aliases may map to one repo) — this list
/// has exactly one entry per unique repo.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CuratedSkillPack {
    pub id: &'static str,
    pub name: &'static str,
    pub description: &'static str,
    pub repo: &'static str,
}

const CURATED_SKILL_PACKS: &[CuratedSkillPack] = &[CuratedSkillPack {
    id: "superpowers",
    name: "Superpowers",
    description: "Curated workflow and development skills",
    repo: "https://github.com/obra/superpowers",
}];

pub fn curated_skill_packs() -> &'static [CuratedSkillPack] {
    CURATED_SKILL_PACKS
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledSkillEntry {
    pub id: String,
    pub name: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledSkillPack {
    pub id: String,
    pub name: String,
    pub source: String,
    pub plugin_id: Option<String>,
    pub installed_at: String,
    pub skills: Vec<InstalledSkillEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct InstalledSkillInfo {
    pub id: String,
    pub name: String,
    pub source: String,
    pub plugin_id: Option<String>,
    pub installed_at: String,
    pub skill_count: usize,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct SkillInstallProvenance {
    source: String,
    plugin_id: Option<String>,
    installed_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ParsedSkillSource {
    repo: String,
    repo_name: String,
}

#[derive(Debug, Clone)]
struct SkillDescriptor {
    display_name: String,
    normalized_name: String,
    source_dir: PathBuf,
}

#[derive(Debug, Clone)]
struct PackDescriptor {
    plugin_id: String,
    repo_dir: PathBuf,
    manifest: ryuzi_plugin_sdk::PluginManifest,
    manifest_to_write: Option<String>,
}

#[derive(Debug, Clone)]
enum Discovery {
    Single(SkillDescriptor),
    // Boxed: PackDescriptor is ~800 bytes vs Single's ~72
    // (clippy::large_enum_variant).
    Pack(Box<PackDescriptor>),
}

#[derive(Debug, Clone)]
struct InstallRoots {
    config_root: PathBuf,
    skills_root: PathBuf,
    plugins_root: PathBuf,
}

impl InstallRoots {
    fn new(config_root: PathBuf) -> Self {
        Self {
            skills_root: config_root.join("skills"),
            plugins_root: config_root.join("plugins"),
            config_root,
        }
    }

    fn for_user() -> Result<Self> {
        #[cfg(test)]
        if let Some(root) = std::env::var_os("RYUZI_TEST_CONFIG_ROOT") {
            return Ok(Self::new(PathBuf::from(root)));
        }

        let home = dirs::home_dir().ok_or_else(|| anyhow!("could not resolve home directory"))?;
        Ok(Self::new(home.join(".config/ryuzi")))
    }

    fn ensure_exists(&self) -> Result<()> {
        std::fs::create_dir_all(&self.config_root)?;
        std::fs::create_dir_all(&self.skills_root)?;
        std::fs::create_dir_all(&self.plugins_root)?;
        Ok(())
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CodexPluginJson {
    name: Option<String>,
    version: Option<String>,
    description: Option<String>,
    homepage: Option<String>,
    repository: Option<String>,
    skills: Option<String>,
    author: Option<CodexPluginAuthor>,
    interface: Option<CodexPluginInterface>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default)]
struct CodexPluginAuthor {
    name: Option<String>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct CodexPluginInterface {
    display_name: Option<String>,
    short_description: Option<String>,
    developer_name: Option<String>,
    website_url: Option<String>,
}

#[async_trait::async_trait]
trait RepoCloner {
    /// Clone `source` into `dest`. Returns the resolved commit SHA when it can
    /// be determined (`None` for test doubles that don't produce a git repo).
    async fn clone_repo(&self, source: &ParsedSkillSource, dest: &Path) -> Result<Option<String>>;
}

struct GitRepoCloner;

#[async_trait::async_trait]
impl RepoCloner for GitRepoCloner {
    async fn clone_repo(&self, source: &ParsedSkillSource, dest: &Path) -> Result<Option<String>> {
        let mut cmd = tokio::process::Command::new("git");
        cmd.arg("clone")
            .arg("--depth")
            .arg("1")
            .arg(&source.repo)
            .arg(dest);
        crate::process_util::no_window(&mut cmd);
        let output = cmd
            .output()
            .await
            .with_context(|| format!("failed to spawn git clone for {}", source.repo))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            if stderr.is_empty() {
                bail!("git clone failed for {}", source.repo);
            }
            bail!("git clone failed for {}: {}", source.repo, stderr);
        }
        // The clone still has `.git` at this point; `copy_dir_recursive`
        // strips it later when the tree is installed. Resolve HEAD now while
        // it's still available.
        let head = tokio::process::Command::new("git")
            .arg("-C")
            .arg(dest)
            .arg("rev-parse")
            .arg("HEAD")
            .output()
            .await
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
            .filter(|s| !s.is_empty());
        Ok(head)
    }
}

pub async fn install_skill_source(source: &str) -> Result<InstalledSkillPack> {
    let roots = InstallRoots::for_user()?;
    let cloner = GitRepoCloner;
    install_skill_source_with(source, &roots, &cloner).await
}

pub fn list_installed_skills() -> Result<Vec<InstalledSkillInfo>> {
    let roots = InstallRoots::for_user()?;
    list_installed_skills_in(&roots)
}

pub fn remove_installed_skill(id: &str) -> Result<()> {
    let roots = InstallRoots::for_user()?;
    remove_installed_skill_in(&roots, id)
}

pub async fn refresh_installed_skill(id: &str) -> Result<InstalledSkillPack> {
    let roots = InstallRoots::for_user()?;
    let cloner = GitRepoCloner;
    refresh_installed_skill_with(id, &roots, &cloner).await
}

async fn install_skill_source_with(
    source: &str,
    roots: &InstallRoots,
    cloner: &impl RepoCloner,
) -> Result<InstalledSkillPack> {
    roots.ensure_exists()?;
    let source = parse_skill_source(source)?;
    let temp = tempfile::tempdir()?;
    let repo_dir = temp.path().join("repo");
    // Bound but unused until a later task threads it into the install ledger.
    let _commit = cloner.clone_repo(&source, &repo_dir).await?;
    let discovered = discover_install_target(&repo_dir, &source)?;
    match discovered {
        Discovery::Single(skill) => install_single_skill(roots, &source, skill),
        Discovery::Pack(pack) => install_plugin_pack(roots, &source, *pack),
    }
}

async fn refresh_installed_skill_with(
    id: &str,
    roots: &InstallRoots,
    cloner: &impl RepoCloner,
) -> Result<InstalledSkillPack> {
    let installed = read_installed_pack(roots, id)?;
    let refreshed = install_skill_source_with(&installed.source, roots, cloner).await?;
    remove_stale_refresh_artifacts(roots, &installed, &refreshed)?;
    Ok(refreshed)
}

fn remove_stale_refresh_artifacts(
    roots: &InstallRoots,
    installed: &InstalledSkillPack,
    refreshed: &InstalledSkillPack,
) -> Result<()> {
    match (
        installed.plugin_id.as_deref(),
        refreshed.plugin_id.as_deref(),
    ) {
        (Some(previous_plugin_id), Some(refreshed_plugin_id)) => {
            if previous_plugin_id != refreshed_plugin_id {
                remove_all_artifacts_for_identity(roots, previous_plugin_id)?;
            }
        }
        (Some(previous_plugin_id), None) => {
            if previous_plugin_id == refreshed.id {
                remove_checked_dir(&roots.plugins_root, previous_plugin_id)?;
                for skill_id in materialized_skill_ids_for_plugin(roots, previous_plugin_id)? {
                    remove_checked_dir(&roots.skills_root, &skill_id)?;
                }
            } else {
                remove_all_artifacts_for_identity(roots, previous_plugin_id)?;
            }
        }
        (None, Some(refreshed_plugin_id)) => {
            if installed.id != refreshed_plugin_id {
                remove_all_artifacts_for_identity(roots, &installed.id)?;
            } else {
                remove_checked_dir(&roots.skills_root, &installed.id)?;
            }
        }
        (None, None) => {
            if refreshed.id != installed.id {
                remove_all_artifacts_for_identity(roots, &installed.id)?;
            }
        }
    }

    Ok(())
}

fn list_installed_skills_in(roots: &InstallRoots) -> Result<Vec<InstalledSkillInfo>> {
    let mut infos = collect_installed_packs(roots)?
        .into_iter()
        .map(|pack| InstalledSkillInfo {
            id: pack.id,
            name: pack.name,
            source: pack.source,
            plugin_id: pack.plugin_id,
            installed_at: pack.installed_at,
            skill_count: pack.skills.len(),
        })
        .collect::<Vec<_>>();
    infos.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    Ok(infos)
}

fn remove_installed_skill_in(roots: &InstallRoots, id: &str) -> Result<()> {
    read_installed_pack(roots, id)?;
    remove_all_artifacts_for_identity(roots, id)
}

fn install_single_skill(
    roots: &InstallRoots,
    source: &ParsedSkillSource,
    skill: SkillDescriptor,
) -> Result<InstalledSkillPack> {
    let installed_at = now_rfc3339();
    let target = checked_child(&roots.skills_root, &skill.normalized_name)?;
    replace_dir_from(&skill.source_dir, &target)?;
    write_provenance(
        &target.join(PROVENANCE_FILE),
        &SkillInstallProvenance {
            source: source.repo.clone(),
            plugin_id: None,
            installed_at: installed_at.clone(),
        },
    )?;
    remove_stale_plugin_pack_artifacts_for_single_install(roots, &skill.normalized_name)?;
    Ok(installed_single_skill_pack(
        &skill,
        &source.repo,
        installed_at,
    ))
}

fn install_plugin_pack(
    roots: &InstallRoots,
    source: &ParsedSkillSource,
    pack: PackDescriptor,
) -> Result<InstalledSkillPack> {
    let plugin_target = checked_child(&roots.plugins_root, &pack.plugin_id)?;
    replace_dir_from(&pack.repo_dir, &plugin_target)?;
    if let Some(text) = &pack.manifest_to_write {
        std::fs::write(plugin_target.join("ryuzi-plugin.toml"), text)?;
    }

    let existing = materialized_skill_ids_for_plugin(roots, &pack.plugin_id)?;
    let materialized = materialized_skills_from_manifest(&plugin_target, &pack.manifest)?;
    let desired = materialized
        .iter()
        .map(|skill| format!("{}--{}", pack.plugin_id, skill.normalized_name))
        .collect::<HashSet<_>>();
    let installed_at = now_rfc3339();

    // Skill-pack provenance in the plugin directory itself: the loader
    // (`crate::plugins::load_skill_pack_plugins_from`) only registers
    // directories carrying this stamp (or heals legacy installs from the
    // materialized skills' provenance below the skills root).
    write_provenance(
        &plugin_target.join(PROVENANCE_FILE),
        &SkillInstallProvenance {
            source: source.repo.clone(),
            plugin_id: Some(pack.plugin_id.clone()),
            installed_at: installed_at.clone(),
        },
    )?;

    for skill in &materialized {
        let target_id = format!("{}--{}", pack.plugin_id, skill.normalized_name);
        let target = checked_child(&roots.skills_root, &target_id)?;
        replace_dir_from(&skill.source_dir, &target)?;
        write_provenance(
            &target.join(PROVENANCE_FILE),
            &SkillInstallProvenance {
                source: source.repo.clone(),
                plugin_id: Some(pack.plugin_id.clone()),
                installed_at: installed_at.clone(),
            },
        )?;
    }

    for stale in existing {
        if !desired.contains(&stale) {
            remove_checked_dir(&roots.skills_root, &stale)?;
        }
    }

    remove_stale_single_skill_artifact_for_plugin_install(roots, &pack.plugin_id)?;

    Ok(installed_plugin_pack(
        &pack,
        &source.repo,
        &materialized,
        installed_at,
    ))
}

fn installed_single_skill_pack(
    skill: &SkillDescriptor,
    source: &str,
    installed_at: String,
) -> InstalledSkillPack {
    InstalledSkillPack {
        id: skill.normalized_name.clone(),
        name: skill.display_name.clone(),
        source: source.to_string(),
        plugin_id: None,
        installed_at,
        skills: vec![InstalledSkillEntry {
            id: skill.normalized_name.clone(),
            name: skill.display_name.clone(),
        }],
    }
}

fn installed_plugin_pack(
    pack: &PackDescriptor,
    source: &str,
    materialized: &[SkillDescriptor],
    installed_at: String,
) -> InstalledSkillPack {
    let mut skills = materialized
        .iter()
        .map(|skill| InstalledSkillEntry {
            id: format!("{}--{}", pack.plugin_id, skill.normalized_name),
            name: skill.display_name.clone(),
        })
        .collect::<Vec<_>>();
    skills.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    InstalledSkillPack {
        id: pack.plugin_id.clone(),
        name: pack.manifest.name.clone(),
        source: source.to_string(),
        plugin_id: Some(pack.plugin_id.clone()),
        installed_at,
        skills,
    }
}

fn remove_stale_plugin_pack_artifacts_for_single_install(
    roots: &InstallRoots,
    skill_id: &str,
) -> Result<()> {
    remove_checked_dir(&roots.plugins_root, skill_id)?;
    for materialized_skill_id in materialized_skill_ids_for_plugin(roots, skill_id)? {
        remove_checked_dir(&roots.skills_root, &materialized_skill_id)?;
    }
    Ok(())
}

fn remove_stale_single_skill_artifact_for_plugin_install(
    roots: &InstallRoots,
    plugin_id: &str,
) -> Result<()> {
    remove_checked_dir(&roots.skills_root, plugin_id)
}

fn parse_skill_source(source: &str) -> Result<ParsedSkillSource> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        bail!("unsupported skill source: empty input");
    }

    let lower = trimmed.to_ascii_lowercase();
    if let Some((_, repo)) = CURATED_SKILL_SOURCES
        .iter()
        .find(|(alias, _)| *alias == lower)
    {
        return parsed_source_from_repo(repo);
    }

    if trimmed.starts_with("https://github.com/") {
        return parsed_source_from_repo(trimmed);
    }

    let parts = trimmed.split('/').collect::<Vec<_>>();
    if parts.len() == 2
        && parts
            .iter()
            .all(|part| !part.trim().is_empty() && !part.contains(char::is_whitespace))
    {
        return parsed_source_from_repo(&format!("https://github.com/{trimmed}"));
    }

    bail!("unsupported skill source: {trimmed}")
}

fn parsed_source_from_repo(repo: &str) -> Result<ParsedSkillSource> {
    let canonical = canonical_github_repo(repo)?;
    let repo_name = canonical
        .rsplit('/')
        .next()
        .ok_or_else(|| anyhow!("could not determine repository name for {canonical}"))?
        .to_string();
    Ok(ParsedSkillSource {
        repo: canonical,
        repo_name,
    })
}

fn canonical_github_repo(repo: &str) -> Result<String> {
    let raw = repo.trim().trim_end_matches('/');
    let Some(rest) = raw.strip_prefix("https://github.com/") else {
        bail!("unsupported skill source: {repo}");
    };
    let rest = rest.trim_end_matches(".git");
    let parts = rest
        .split('/')
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    if parts.len() != 2 || parts.iter().any(|part| part.contains(char::is_whitespace)) {
        bail!("unsupported skill source: {repo}");
    }
    Ok(format!("https://github.com/{}/{}", parts[0], parts[1]))
}

fn discover_install_target(repo_dir: &Path, source: &ParsedSkillSource) -> Result<Discovery> {
    let plugin_json_path = repo_dir.join(".codex-plugin/plugin.json");
    if plugin_json_path.is_file() {
        return Ok(Discovery::Pack(Box::new(
            discover_plugin_pack_from_plugin_json(repo_dir, source, &plugin_json_path)?,
        )));
    }

    if repo_dir.join("SKILL.md").is_file() {
        return Ok(Discovery::Single(read_skill_descriptor(repo_dir)?));
    }

    let skills = scan_skill_root(&repo_dir.join("skills"))?;
    if !skills.is_empty() {
        return Ok(Discovery::Pack(Box::new(discover_bare_plugin_pack(
            repo_dir, source, skills,
        )?)));
    }

    bail!("no installable skill found in repo — checked .codex-plugin/plugin.json, SKILL.md, and skills/*/SKILL.md")
}

fn discover_plugin_pack_from_plugin_json(
    repo_dir: &Path,
    source: &ParsedSkillSource,
    plugin_json_path: &Path,
) -> Result<PackDescriptor> {
    let plugin_json: CodexPluginJson =
        serde_json::from_str(&std::fs::read_to_string(plugin_json_path)?)
            .with_context(|| format!("invalid plugin.json at {}", plugin_json_path.display()))?;

    let existing_manifest_path = repo_dir.join("ryuzi-plugin.toml");
    if existing_manifest_path.is_file() {
        let text = std::fs::read_to_string(&existing_manifest_path)?;
        let manifest_dir = existing_manifest_path.parent().unwrap_or(repo_dir);
        let mut manifest =
            ryuzi_plugin_sdk::PluginManifest::from_toml(&text).with_context(|| {
                format!(
                    "invalid preserved plugin manifest at {}",
                    existing_manifest_path.display()
                )
            })?;
        if manifest.skills.is_empty() {
            let default_skill_root = plugin_json.skills.as_deref().unwrap_or("./skills/");
            let default_skills = discover_skill_descriptors(repo_dir, default_skill_root)?;
            manifest.skills = manifest_skill_defs(repo_dir, &default_skills)?;
            let manifest_text = toml::to_string_pretty(&manifest)?;
            return Ok(PackDescriptor {
                plugin_id: manifest.id.clone(),
                repo_dir: repo_dir.to_path_buf(),
                manifest,
                manifest_to_write: Some(manifest_text),
            });
        }
        let materialized = materialized_skills_from_manifest(manifest_dir, &manifest)?;
        if materialized.is_empty() {
            bail!(
                "plugin manifest at {} does not resolve any installable skills",
                existing_manifest_path.display()
            );
        }
        return Ok(PackDescriptor {
            plugin_id: manifest.id.clone(),
            repo_dir: repo_dir.to_path_buf(),
            manifest,
            manifest_to_write: None,
        });
    }

    let default_skill_root = plugin_json.skills.as_deref().unwrap_or("./skills/");
    let default_skills = discover_skill_descriptors(repo_dir, default_skill_root)?;

    let plugin_id = normalize_name(
        plugin_json
            .name
            .as_deref()
            .unwrap_or(source.repo_name.as_str()),
    );
    let name = plugin_json
        .interface
        .as_ref()
        .and_then(|value| value.display_name.clone())
        .or_else(|| plugin_json.name.clone())
        .unwrap_or_else(|| source.repo_name.clone());
    let description = plugin_json
        .description
        .clone()
        .or_else(|| {
            plugin_json
                .interface
                .as_ref()
                .and_then(|value| value.short_description.clone())
        })
        .unwrap_or_default();
    let publisher = plugin_json
        .interface
        .as_ref()
        .and_then(|value| value.developer_name.clone())
        .or_else(|| {
            plugin_json
                .author
                .as_ref()
                .and_then(|author| author.name.clone())
        })
        .unwrap_or_default();
    let homepage = plugin_json.homepage.clone().or_else(|| {
        plugin_json
            .interface
            .as_ref()
            .and_then(|value| value.website_url.clone())
    });
    let manifest = generated_plugin_manifest(
        &plugin_id,
        &name,
        plugin_json.version.as_deref().unwrap_or_default(),
        &description,
        &publisher,
        homepage,
        manifest_skill_defs(repo_dir, &default_skills)?,
    )?;
    Ok(PackDescriptor {
        plugin_id,
        repo_dir: repo_dir.to_path_buf(),
        manifest_to_write: Some(toml::to_string_pretty(&manifest)?),
        manifest,
    })
}

fn discover_bare_plugin_pack(
    repo_dir: &Path,
    source: &ParsedSkillSource,
    skills: Vec<SkillDescriptor>,
) -> Result<PackDescriptor> {
    let plugin_id = normalize_name(&source.repo_name);
    let manifest = generated_plugin_manifest(
        &plugin_id,
        &source.repo_name,
        "",
        "",
        "",
        None,
        manifest_skill_defs(repo_dir, &skills)?,
    )?;
    Ok(PackDescriptor {
        plugin_id: plugin_id.clone(),
        repo_dir: repo_dir.to_path_buf(),
        manifest_to_write: Some(toml::to_string_pretty(&manifest)?),
        manifest,
    })
}

fn generated_plugin_manifest(
    plugin_id: &str,
    name: &str,
    version: &str,
    description: &str,
    publisher: &str,
    homepage: Option<String>,
    skills: Vec<ryuzi_plugin_sdk::SkillDef>,
) -> Result<ryuzi_plugin_sdk::PluginManifest> {
    let manifest = ryuzi_plugin_sdk::PluginManifest {
        contract: ryuzi_plugin_sdk::CONTRACT_VERSION,
        id: plugin_id.to_string(),
        name: name.to_string(),
        version: version.to_string(),
        publisher: publisher.to_string(),
        description: description.to_string(),
        homepage,
        icon: None,
        categories: vec![],
        verified: false,
        experimental: false,
        auth: None,
        settings: vec![],
        mcp: vec![],
        skills,
        provider: None,
        runtime: None,
    };
    manifest.validate()?;
    Ok(manifest)
}

fn manifest_skill_defs(
    repo_dir: &Path,
    skills: &[SkillDescriptor],
) -> Result<Vec<ryuzi_plugin_sdk::SkillDef>> {
    let mut out = Vec::with_capacity(skills.len());
    for skill in skills {
        out.push(ryuzi_plugin_sdk::SkillDef {
            name: skill.display_name.clone(),
            description: String::new(),
            path: relative_path_string(repo_dir, &skill.source_dir)?,
        });
    }
    Ok(out)
}

fn materialized_skills_from_manifest(
    base_dir: &Path,
    manifest: &ryuzi_plugin_sdk::PluginManifest,
) -> Result<Vec<SkillDescriptor>> {
    let mut out = Vec::new();
    for skill in &manifest.skills {
        let skill_dir = resolve_within(base_dir, &skill.path)?;
        if !skill_dir.join("SKILL.md").is_file() {
            bail!("plugin skill path {} does not contain SKILL.md", skill.path);
        }
        let mut descriptor = read_skill_descriptor(&skill_dir)?;
        if descriptor.display_name.is_empty() {
            descriptor.display_name = skill.name.clone();
        }
        if descriptor.normalized_name.is_empty() {
            descriptor.normalized_name = normalize_name(&skill.name);
        }
        out.push(descriptor);
    }
    if out.is_empty() {
        bail!("plugin manifest declares no installable skills");
    }
    Ok(out)
}

fn discover_skill_descriptors(repo_dir: &Path, rel: &str) -> Result<Vec<SkillDescriptor>> {
    let root = resolve_within(repo_dir, rel)?;
    if root.join("SKILL.md").is_file() {
        return Ok(vec![read_skill_descriptor(&root)?]);
    }
    let skills = scan_skill_root(&root)?;
    if skills.is_empty() {
        bail!("no installable skills found under {}", root.display());
    }
    Ok(skills)
}

fn scan_skill_root(root: &Path) -> Result<Vec<SkillDescriptor>> {
    let mut dirs = Vec::new();
    let Ok(entries) = std::fs::read_dir(root) else {
        return Ok(vec![]);
    };
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let path = entry.path();
        if path.join("SKILL.md").is_file() {
            dirs.push(read_skill_descriptor(&path)?);
        }
    }
    dirs.sort_by(|a, b| a.normalized_name.cmp(&b.normalized_name));
    Ok(dirs)
}

fn read_skill_descriptor(skill_dir: &Path) -> Result<SkillDescriptor> {
    let text = std::fs::read_to_string(skill_dir.join("SKILL.md"))
        .with_context(|| format!("missing SKILL.md in {}", skill_dir.display()))?;
    let (frontmatter, _) = crate::harness::native::agents::split_frontmatter_pub(&text);
    let fallback = skill_dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("skill")
        .to_string();
    let display_name = frontmatter
        .into_iter()
        .find_map(|(key, value)| (key == "name").then_some(value))
        .unwrap_or(fallback);
    Ok(SkillDescriptor {
        normalized_name: normalize_name(&display_name),
        display_name,
        source_dir: skill_dir.to_path_buf(),
    })
}

fn collect_installed_packs(roots: &InstallRoots) -> Result<Vec<InstalledSkillPack>> {
    let Ok(entries) = std::fs::read_dir(&roots.skills_root) else {
        return Ok(vec![]);
    };

    let mut grouped: HashMap<String, InstalledSkillPack> = HashMap::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let skill_dir = entry.path();
        let provenance_path = skill_dir.join(PROVENANCE_FILE);
        if !provenance_path.is_file() {
            continue;
        }

        let provenance: SkillInstallProvenance = serde_json::from_str(&std::fs::read_to_string(
            &provenance_path,
        )?)
        .with_context(|| format!("invalid provenance file at {}", provenance_path.display()))?;
        let skill_id = entry.file_name().to_string_lossy().to_string();
        let skill_name = read_skill_descriptor(&skill_dir)?.display_name;
        let group_id = provenance
            .plugin_id
            .clone()
            .unwrap_or_else(|| skill_id.clone());
        let pack_name = match &provenance.plugin_id {
            Some(plugin_id) => {
                plugin_display_name(roots, plugin_id).unwrap_or_else(|| plugin_id.clone())
            }
            None => skill_name.clone(),
        };
        let skill = InstalledSkillEntry {
            id: skill_id,
            name: skill_name,
        };
        let pack = grouped
            .entry(group_id.clone())
            .or_insert_with(|| InstalledSkillPack {
                id: group_id.clone(),
                name: pack_name.clone(),
                source: provenance.source.clone(),
                plugin_id: provenance.plugin_id.clone(),
                installed_at: provenance.installed_at.clone(),
                skills: Vec::new(),
            });
        let is_newer_generation = provenance.installed_at > pack.installed_at;
        let is_same_generation = provenance.installed_at == pack.installed_at;
        if is_newer_generation {
            pack.name = pack_name;
            pack.source = provenance.source;
            pack.plugin_id = provenance.plugin_id;
            pack.installed_at = provenance.installed_at;
            pack.skills.clear();
        }
        if is_same_generation || is_newer_generation {
            pack.skills.push(skill);
        }
    }

    let mut packs = grouped.into_values().collect::<Vec<_>>();
    for pack in &mut packs {
        pack.skills
            .sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    }
    packs.sort_by(|a, b| a.name.cmp(&b.name).then(a.id.cmp(&b.id)));
    Ok(packs)
}

fn read_installed_pack(roots: &InstallRoots, id: &str) -> Result<InstalledSkillPack> {
    collect_installed_packs(roots)?
        .into_iter()
        .find(|pack| pack.id == id)
        .ok_or_else(|| anyhow!("unknown installed skill: {id}"))
}

fn materialized_skill_ids_for_plugin(roots: &InstallRoots, plugin_id: &str) -> Result<Vec<String>> {
    let Ok(entries) = std::fs::read_dir(&roots.skills_root) else {
        return Ok(vec![]);
    };
    let mut ids = Vec::new();
    for entry in entries {
        let entry = entry?;
        if !entry.file_type()?.is_dir() {
            continue;
        }
        let provenance_path = entry.path().join(PROVENANCE_FILE);
        if !provenance_path.is_file() {
            continue;
        }
        let provenance: SkillInstallProvenance = serde_json::from_str(&std::fs::read_to_string(
            &provenance_path,
        )?)
        .with_context(|| format!("invalid provenance file at {}", provenance_path.display()))?;
        if provenance.plugin_id.as_deref() == Some(plugin_id) {
            ids.push(entry.file_name().to_string_lossy().to_string());
        }
    }
    Ok(ids)
}

fn remove_all_artifacts_for_identity(roots: &InstallRoots, id: &str) -> Result<()> {
    remove_checked_dir(&roots.plugins_root, id)?;
    remove_checked_dir(&roots.skills_root, id)?;
    for materialized_skill_id in materialized_skill_ids_for_plugin(roots, id)? {
        remove_checked_dir(&roots.skills_root, &materialized_skill_id)?;
    }
    Ok(())
}

fn plugin_display_name(roots: &InstallRoots, plugin_id: &str) -> Option<String> {
    let manifest_path = roots.plugins_root.join(plugin_id).join("ryuzi-plugin.toml");
    let text = std::fs::read_to_string(manifest_path).ok()?;
    ryuzi_plugin_sdk::PluginManifest::from_toml(&text)
        .ok()
        .map(|manifest| manifest.name)
}

/// Legacy skill packs installed before `install_plugin_pack` stamped
/// `.ryuzi-skill.json` into the plugin directory carry provenance only in
/// their materialized skill dirs under the skills root. When one of those
/// names `plugin_id`, copy that provenance into `plugin_dir` (one-time
/// heal) and return `true`; return `false` when nothing names the plugin
/// (hand-authored manifests — the loader skips them).
pub(crate) fn stamp_legacy_skill_pack_provenance(
    skills_root: &Path,
    plugin_dir: &Path,
    plugin_id: &str,
) -> bool {
    // `install_plugin_pack` always writes packs at `plugins_root/<plugin_id>`
    // (see `checked_child(&roots.plugins_root, &pack.plugin_id)` above), so a
    // legitimate legacy pack's directory name always equals its plugin id.
    // Without this guard, a hand-authored directory under any other name
    // could claim an installed pack's id in its manifest and ride that
    // pack's materialized skills-root provenance to get itself healed and
    // permanently trusted — same-id spoofing. Reject anything whose
    // directory name doesn't match before even looking at the skills root.
    if plugin_dir.file_name().and_then(|n| n.to_str()) != Some(plugin_id) {
        return false;
    }
    let Ok(entries) = std::fs::read_dir(skills_root) else {
        return false;
    };
    for entry in entries.filter_map(Result::ok) {
        let provenance_path = entry.path().join(PROVENANCE_FILE);
        let Ok(text) = std::fs::read_to_string(&provenance_path) else {
            continue;
        };
        let Ok(provenance) = serde_json::from_str::<SkillInstallProvenance>(&text) else {
            continue;
        };
        if provenance.plugin_id.as_deref() != Some(plugin_id) {
            continue;
        }
        if let Err(e) = write_provenance(&plugin_dir.join(PROVENANCE_FILE), &provenance) {
            tracing::warn!(
                "failed to stamp skill-pack provenance into {}: {e}",
                plugin_dir.display()
            );
        }
        return true; // provenance exists — the pack is legit even if the stamp write failed
    }
    false
}

fn resolve_within(base_dir: &Path, rel: &str) -> Result<PathBuf> {
    let base_dir = base_dir
        .canonicalize()
        .with_context(|| format!("missing base directory {}", base_dir.display()))?;
    let target = base_dir.join(rel);
    let target = target
        .canonicalize()
        .with_context(|| format!("missing path {} under {}", rel, base_dir.display()))?;
    if !target.starts_with(&base_dir) {
        bail!("path escapes install root: {rel}");
    }
    Ok(target)
}

fn relative_path_string(base_dir: &Path, target: &Path) -> Result<String> {
    let base_dir = base_dir
        .canonicalize()
        .unwrap_or_else(|_| base_dir.to_path_buf());
    let target = target
        .canonicalize()
        .unwrap_or_else(|_| target.to_path_buf());
    Ok(target
        .strip_prefix(&base_dir)?
        .to_string_lossy()
        .replace('\\', "/"))
}

fn replace_dir_from(source: &Path, target: &Path) -> Result<()> {
    let parent = target
        .parent()
        .ok_or_else(|| anyhow!("install target has no parent: {}", target.display()))?;
    std::fs::create_dir_all(parent)?;
    let staging = parent.join(format!(".tmp-{}", crate::paths::new_id()));
    copy_dir_recursive(source, &staging)?;
    if target.exists() {
        if target.is_dir() {
            std::fs::remove_dir_all(target)?;
        } else {
            std::fs::remove_file(target)?;
        }
    }
    std::fs::rename(&staging, target)?;
    Ok(())
}

fn copy_dir_recursive(source: &Path, dest: &Path) -> Result<()> {
    std::fs::create_dir_all(dest)?;
    for entry in std::fs::read_dir(source)? {
        let entry = entry?;
        let name = entry.file_name();
        if name.to_string_lossy() == ".git" {
            continue;
        }
        let source_path = entry.path();
        let dest_path = dest.join(&name);
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_recursive(&source_path, &dest_path)?;
        } else if file_type.is_file() {
            if let Some(parent) = dest_path.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::copy(&source_path, &dest_path)?;
        }
    }
    Ok(())
}

/// Stable content hash of an installed tree. Walks files in sorted order,
/// hashing each file's relative path and bytes, so the same content always
/// yields the same digest regardless of install location. Excludes `.git`
/// (stripped by `copy_dir_recursive` anyway) and `PROVENANCE_FILE` (written
/// AFTER fingerprinting — including it would make every live-dir comparison a
/// false mismatch). Symlinks/special files are ignored, matching
/// `copy_dir_recursive`.
///
/// Not yet called from production code — a later task wires it into the
/// install ledger for local-edit detection. Allow dead-code on this pair
/// until then so clippy stays clean; both are exercised directly by
/// `fingerprint_is_stable_and_excludes_git_and_stamp` below.
#[allow(dead_code)]
pub(crate) fn fingerprint_dir(dir: &Path) -> Result<String> {
    use sha2::{Digest, Sha256};
    let mut files: Vec<(String, PathBuf)> = Vec::new();
    collect_files_rel(dir, dir, &mut files)?;
    files.sort_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = Sha256::new();
    for (rel, path) in files {
        let bytes = std::fs::read(&path)
            .with_context(|| format!("failed to read {} for fingerprint", path.display()))?;
        hasher.update(rel.as_bytes());
        hasher.update([0u8]);
        hasher.update((bytes.len() as u64).to_le_bytes());
        hasher.update([0u8]);
        hasher.update(&bytes);
    }
    Ok(format!("sha256:{:x}", hasher.finalize()))
}

#[allow(dead_code)]
fn collect_files_rel(base: &Path, dir: &Path, out: &mut Vec<(String, PathBuf)>) -> Result<()> {
    for entry in std::fs::read_dir(dir)?.filter_map(std::result::Result::ok) {
        let path = entry.path();
        let name = entry.file_name();
        if name == ".git" || name == std::ffi::OsStr::new(PROVENANCE_FILE) {
            continue;
        }
        if path.is_dir() {
            collect_files_rel(base, &path, out)?;
        } else if path.is_file() {
            let rel = path
                .strip_prefix(base)
                .unwrap_or(&path)
                .to_string_lossy()
                .replace('\\', "/");
            out.push((rel, path));
        }
    }
    Ok(())
}

fn checked_child(root: &Path, id: &str) -> Result<PathBuf> {
    if !is_safe_id(id) {
        bail!("unsafe install id: {id}");
    }
    Ok(root.join(id))
}

fn remove_checked_dir(root: &Path, id: &str) -> Result<()> {
    let target = checked_child(root, id)?;
    if !target.exists() {
        return Ok(());
    }
    if target.is_dir() {
        std::fs::remove_dir_all(target)?;
    } else {
        std::fs::remove_file(target)?;
    }
    Ok(())
}

fn is_safe_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn write_provenance(path: &Path, provenance: &SkillInstallProvenance) -> Result<()> {
    std::fs::write(path, serde_json::to_string_pretty(provenance)?)?;
    Ok(())
}

fn now_rfc3339() -> String {
    Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true)
}

fn normalize_name(input: &str) -> String {
    let mut out = String::new();
    let mut last_dash = false;
    for ch in input.chars().flat_map(char::to_lowercase) {
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
            last_dash = false;
        } else if !last_dash {
            out.push('-');
            last_dash = true;
        }
    }
    let trimmed = out.trim_matches('-').to_string();
    if trimmed.is_empty() {
        "skill".to_string()
    } else {
        trimmed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct FakeRepoCloner {
        repos: BTreeMap<String, PathBuf>,
        commit: Option<String>,
    }

    #[async_trait::async_trait]
    impl RepoCloner for FakeRepoCloner {
        async fn clone_repo(
            &self,
            source: &ParsedSkillSource,
            dest: &Path,
        ) -> Result<Option<String>> {
            let repo = self
                .repos
                .get(&source.repo)
                .ok_or_else(|| anyhow!("missing fake repo for {}", source.repo))?;
            copy_dir_recursive(repo, dest)?;
            Ok(self.commit.clone())
        }
    }

    fn write_skill(dir: &std::path::Path, name: &str, description: &str, body: &str) {
        std::fs::create_dir_all(dir).unwrap();
        std::fs::write(
            dir.join("SKILL.md"),
            format!("---\nname: {name}\ndescription: {description}\n---\n{body}\n"),
        )
        .unwrap();
    }

    fn write_installed_skill(
        roots: &InstallRoots,
        id: &str,
        name: &str,
        body: &str,
        provenance: SkillInstallProvenance,
    ) {
        let dir = roots.skills_root.join(id);
        write_skill(&dir, name, "Installed skill", body);
        write_provenance(&dir.join(PROVENANCE_FILE), &provenance).unwrap();
    }

    #[test]
    fn parse_skill_source_resolves_curated_and_github_inputs() {
        assert_eq!(
            parse_skill_source("obra/superpowers").unwrap().repo,
            "https://github.com/obra/superpowers"
        );
        assert_eq!(
            parse_skill_source("https://github.com/obra/superpowers")
                .unwrap()
                .repo,
            "https://github.com/obra/superpowers"
        );
        assert_eq!(
            parse_skill_source("superpowers").unwrap().repo,
            "https://github.com/obra/superpowers"
        );
    }

    #[test]
    fn curated_skill_packs_are_deduped_and_resolvable() {
        let packs = curated_skill_packs();
        assert_eq!(packs.len(), 1, "one unique curated repo today");
        let sp = &packs[0];
        assert_eq!(sp.id, "superpowers");
        assert_eq!(sp.name, "Superpowers");
        assert_eq!(sp.repo, "https://github.com/obra/superpowers");
        // Every curated pack id must resolve through the normal source parser.
        assert_eq!(parse_skill_source(sp.id).unwrap().repo, sp.repo);
    }

    #[tokio::test]
    async fn install_single_skill_repo_copies_skill_and_records_provenance() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write_skill(
            repo.path(),
            "My Skill",
            "Does one thing well",
            "Use this for the thing.",
        );
        std::fs::write(repo.path().join("notes.txt"), "hello").unwrap();

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/acme/my-skill".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let pack = install_skill_source_with("https://github.com/acme/my-skill", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(pack.id, "my-skill");
        assert_eq!(pack.skills.len(), 1);
        assert_eq!(pack.skills[0].id, "my-skill");

        let installed = roots.skills_root.join("my-skill");
        assert!(installed.join("SKILL.md").is_file());
        assert!(installed.join("notes.txt").is_file());

        let provenance: SkillInstallProvenance = serde_json::from_str(
            &std::fs::read_to_string(installed.join(".ryuzi-skill.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(provenance.source, "https://github.com/acme/my-skill");
        assert_eq!(provenance.plugin_id, None);

        let listed = list_installed_skills_in(&roots).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "my-skill");
        assert_eq!(listed[0].skill_count, 1);
    }

    #[test]
    fn stamp_legacy_skill_pack_provenance_heals_from_materialized_skills() {
        let config = tempfile::tempdir().unwrap();
        let roots = InstallRoots::new(config.path().to_path_buf());
        roots.ensure_exists().unwrap();
        let plugin_dir = roots.plugins_root.join("legacy-pack");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        write_installed_skill(
            &roots,
            "legacy-pack--triage",
            "triage",
            "Old pack skill.",
            SkillInstallProvenance {
                source: "https://github.com/acme/legacy-pack".to_string(),
                plugin_id: Some("legacy-pack".to_string()),
                installed_at: "2026-01-01T00:00:00.000Z".to_string(),
            },
        );

        assert!(stamp_legacy_skill_pack_provenance(
            &roots.skills_root,
            &plugin_dir,
            "legacy-pack"
        ));
        let stamped: SkillInstallProvenance = serde_json::from_str(
            &std::fs::read_to_string(plugin_dir.join(PROVENANCE_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(stamped.plugin_id.as_deref(), Some("legacy-pack"));
        assert_eq!(stamped.source, "https://github.com/acme/legacy-pack");
    }

    #[test]
    fn stamp_legacy_skill_pack_provenance_rejects_plugin_without_materialized_provenance() {
        let config = tempfile::tempdir().unwrap();
        let roots = InstallRoots::new(config.path().to_path_buf());
        roots.ensure_exists().unwrap();
        let plugin_dir = roots.plugins_root.join("hand-authored");
        std::fs::create_dir_all(&plugin_dir).unwrap();

        assert!(!stamp_legacy_skill_pack_provenance(
            &roots.skills_root,
            &plugin_dir,
            "hand-authored"
        ));
        assert!(!plugin_dir.join(PROVENANCE_FILE).exists());
    }

    #[test]
    fn stamp_legacy_skill_pack_provenance_rejects_dir_name_spoofing_an_installed_id() {
        // `install_plugin_pack` always writes packs at `plugins_root/<plugin_id>`,
        // so a directory named anything else claiming a real pack's id via its
        // manifest — even when that id's materialized skills-root provenance is
        // genuine — must not be healed or trusted.
        let config = tempfile::tempdir().unwrap();
        let roots = InstallRoots::new(config.path().to_path_buf());
        roots.ensure_exists().unwrap();
        let plugin_dir = roots.plugins_root.join("impostor");
        std::fs::create_dir_all(&plugin_dir).unwrap();
        write_installed_skill(
            &roots,
            "acme-user--triage",
            "triage",
            "Real pack skill.",
            SkillInstallProvenance {
                source: "https://github.com/acme/acme-user".to_string(),
                plugin_id: Some("acme-user".to_string()),
                installed_at: "2026-01-01T00:00:00.000Z".to_string(),
            },
        );

        assert!(!stamp_legacy_skill_pack_provenance(
            &roots.skills_root,
            &plugin_dir,
            "acme-user"
        ));
        assert!(!plugin_dir.join(PROVENANCE_FILE).exists());
    }

    #[tokio::test]
    async fn install_plugin_pack_copies_repo_writes_manifest_and_materializes_skills() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "superpowers",
                "description": "Curated skill pack",
                "homepage": "https://github.com/obra/superpowers",
                "repository": "https://github.com/obra/superpowers",
                "author": { "name": "OpenAI" },
                "skills": "./skills/",
                "interface": {
                    "displayName": "Superpowers",
                    "developerName": "OpenAI",
                    "shortDescription": "Curated skills"
                }
            })
            .to_string(),
        )
        .unwrap();
        write_skill(
            &repo.path().join("skills/brainstorming"),
            "brainstorming",
            "Explore ideas",
            "Ask one question at a time.",
        );
        write_skill(
            &repo.path().join("skills/test-driven-development"),
            "test-driven-development",
            "Write tests first",
            "Red, green, refactor.",
        );

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let pack = install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(pack.id, "superpowers");
        assert_eq!(pack.name, "Superpowers");
        assert_eq!(pack.skills.len(), 2);

        let plugin_dir = roots.plugins_root.join("superpowers");
        assert!(plugin_dir.join(".codex-plugin/plugin.json").is_file());
        assert!(plugin_dir.join("ryuzi-plugin.toml").is_file());

        let pack_provenance: SkillInstallProvenance = serde_json::from_str(
            &std::fs::read_to_string(plugin_dir.join(PROVENANCE_FILE)).unwrap(),
        )
        .unwrap();
        assert_eq!(
            pack_provenance.source,
            "https://github.com/obra/superpowers"
        );
        assert_eq!(pack_provenance.plugin_id.as_deref(), Some("superpowers"));

        let manifest = ryuzi_plugin_sdk::PluginManifest::from_toml(
            &std::fs::read_to_string(plugin_dir.join("ryuzi-plugin.toml")).unwrap(),
        )
        .unwrap();
        assert_eq!(manifest.id, "superpowers");
        assert_eq!(manifest.skills.len(), 2);

        let brainstorming = roots.skills_root.join("superpowers--brainstorming");
        let tdd = roots
            .skills_root
            .join("superpowers--test-driven-development");
        assert!(brainstorming.join("SKILL.md").is_file());
        assert!(tdd.join("SKILL.md").is_file());

        let listed = list_installed_skills_in(&roots).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "superpowers");
        assert_eq!(listed[0].skill_count, 2);
    }

    #[tokio::test]
    async fn install_single_skill_returns_fresh_shape_when_same_id_pack_artifacts_exist() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write_skill(repo.path(), "superpowers", "Explore ideas", "Fresh single.");

        let roots = InstallRoots::new(config.path().to_path_buf());
        std::fs::create_dir_all(roots.plugins_root.join("superpowers")).unwrap();
        std::fs::write(
            roots
                .plugins_root
                .join("superpowers")
                .join("ryuzi-plugin.toml"),
            r#"
contract = 1
id = "superpowers"
name = "Superpowers"

[[skills]]
name = "brainstorming"
description = "Explore ideas"
path = "skills/brainstorming"
"#
            .trim_start(),
        )
        .unwrap();
        write_skill(
            &roots.skills_root.join("superpowers--brainstorming"),
            "brainstorming",
            "Explore ideas",
            "Stale pack skill.",
        );
        write_provenance(
            &roots
                .skills_root
                .join("superpowers--brainstorming")
                .join(PROVENANCE_FILE),
            &SkillInstallProvenance {
                source: "https://github.com/obra/superpowers".to_string(),
                plugin_id: Some("superpowers".to_string()),
                installed_at: "9999-12-31T23:59:59.999Z".to_string(),
            },
        )
        .unwrap();

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let pack = install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(pack.id, "superpowers");
        assert_eq!(pack.plugin_id, None);
        assert_eq!(pack.skills.len(), 1);
        assert_eq!(
            pack.skills,
            vec![InstalledSkillEntry {
                id: "superpowers".to_string(),
                name: "superpowers".to_string(),
            }]
        );
        assert!(!roots.plugins_root.join("superpowers").exists());
        assert!(!roots
            .skills_root
            .join("superpowers--brainstorming")
            .exists());
        assert!(roots
            .skills_root
            .join("superpowers")
            .join("SKILL.md")
            .is_file());

        let listed = list_installed_skills_in(&roots).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "superpowers");
        assert_eq!(listed[0].plugin_id, None);
        assert_eq!(listed[0].skill_count, 1);
    }

    #[tokio::test]
    async fn install_plugin_pack_returns_fresh_shape_when_same_id_single_artifact_exists() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "superpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Superpowers" }
            })
            .to_string(),
        )
        .unwrap();
        write_skill(
            &repo.path().join("skills/brainstorming"),
            "brainstorming",
            "Explore ideas",
            "Fresh pack skill.",
        );

        let roots = InstallRoots::new(config.path().to_path_buf());
        write_skill(
            &roots.skills_root.join("superpowers"),
            "superpowers",
            "Explore ideas",
            "Stale single skill.",
        );
        write_provenance(
            &roots.skills_root.join("superpowers").join(PROVENANCE_FILE),
            &SkillInstallProvenance {
                source: "https://github.com/obra/superpowers".to_string(),
                plugin_id: None,
                installed_at: "9999-12-31T23:59:59.999Z".to_string(),
            },
        )
        .unwrap();

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let pack = install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(pack.id, "superpowers");
        assert_eq!(pack.plugin_id.as_deref(), Some("superpowers"));
        assert_eq!(pack.name, "Superpowers");
        assert_eq!(
            pack.skills,
            vec![InstalledSkillEntry {
                id: "superpowers--brainstorming".to_string(),
                name: "brainstorming".to_string(),
            }]
        );
        assert!(!roots.skills_root.join("superpowers").exists());
        assert!(roots.plugins_root.join("superpowers").exists());
        assert!(roots
            .skills_root
            .join("superpowers--brainstorming")
            .join("SKILL.md")
            .is_file());

        let listed = list_installed_skills_in(&roots).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "superpowers");
        assert_eq!(listed[0].plugin_id.as_deref(), Some("superpowers"));
        assert_eq!(listed[0].skill_count, 1);
    }

    #[tokio::test]
    async fn install_plugin_pack_preserves_existing_manifest_with_custom_skill_paths() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "superpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Superpowers" }
            })
            .to_string(),
        )
        .unwrap();
        write_skill(
            &repo.path().join("bundled/brainstorming"),
            "brainstorming",
            "Explore ideas",
            "Custom layout.",
        );
        let preserved_manifest = r#"
contract = 1
id = "superpowers"
name = "Superpowers"

[[skills]]
name = "brainstorming"
description = "Explore ideas"
path = "bundled/brainstorming"
"#
        .trim_start();
        std::fs::write(repo.path().join("ryuzi-plugin.toml"), preserved_manifest).unwrap();

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let pack = install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(pack.id, "superpowers");
        assert_eq!(pack.skills.len(), 1);
        assert_eq!(pack.skills[0].id, "superpowers--brainstorming");

        let plugin_dir = roots.plugins_root.join("superpowers");
        assert_eq!(
            std::fs::read_to_string(plugin_dir.join("ryuzi-plugin.toml")).unwrap(),
            preserved_manifest
        );
        assert!(plugin_dir.join("bundled/brainstorming/SKILL.md").is_file());
        assert!(roots
            .skills_root
            .join("superpowers--brainstorming")
            .join("SKILL.md")
            .is_file());
    }

    #[tokio::test]
    async fn invalid_preserved_manifest_blocks_plugin_json_fallback_install() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "superpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Superpowers" }
            })
            .to_string(),
        )
        .unwrap();
        write_skill(
            &repo.path().join("skills/brainstorming"),
            "brainstorming",
            "Explore ideas",
            "Default layout.",
        );
        write_skill(
            &repo.path().join("bundled/test-driven-development"),
            "test-driven-development",
            "Write tests first",
            "Custom layout.",
        );
        std::fs::write(
            repo.path().join("ryuzi-plugin.toml"),
            r#"
contract = 1
id = "superpowers"
name = "Superpowers"

[[skills]]
name = "test-driven-development"
description = "Write tests first"
path = 123
"#
            .trim_start(),
        )
        .unwrap();

        let source = parse_skill_source("superpowers").unwrap();
        let discovery_err = discover_plugin_pack_from_plugin_json(
            repo.path(),
            &source,
            &repo.path().join(".codex-plugin/plugin.json"),
        )
        .expect_err("invalid preserved manifest should be authoritative");
        assert!(discovery_err.to_string().contains("ryuzi-plugin.toml"));

        let mut repos = BTreeMap::new();
        repos.insert(source.repo.clone(), repo.path().to_path_buf());
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let install_err = install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .expect_err("install should fail when preserved manifest is invalid");
        assert!(install_err.to_string().contains("ryuzi-plugin.toml"));
        assert!(list_installed_skills_in(&roots).unwrap().is_empty());
        assert!(!roots.plugins_root.join("superpowers").exists());
        assert!(!roots
            .skills_root
            .join("superpowers--brainstorming")
            .exists());
        assert!(!roots
            .skills_root
            .join("superpowers--test-driven-development")
            .exists());
    }

    #[tokio::test]
    async fn install_bare_plugin_pack_discovers_skills_root_entries() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write_skill(
            &repo.path().join("skills/alpha"),
            "alpha",
            "First skill",
            "Alpha body.",
        );
        write_skill(
            &repo.path().join("skills/beta"),
            "beta",
            "Second skill",
            "Beta body.",
        );

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/acme/toolbox".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let pack = install_skill_source_with("acme/toolbox", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(pack.id, "toolbox");
        assert_eq!(pack.skills.len(), 2);
        assert!(roots
            .plugins_root
            .join("toolbox/ryuzi-plugin.toml")
            .is_file());
        assert!(roots
            .skills_root
            .join("toolbox--alpha")
            .join("SKILL.md")
            .is_file());
        assert!(roots
            .skills_root
            .join("toolbox--beta")
            .join("SKILL.md")
            .is_file());
    }

    #[tokio::test]
    async fn refresh_plugin_pack_replaces_materialized_skill_set() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "superpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Superpowers" }
            })
            .to_string(),
        )
        .unwrap();
        write_skill(
            &repo.path().join("skills/brainstorming"),
            "brainstorming",
            "Explore ideas",
            "v1",
        );

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        std::fs::remove_dir_all(repo.path().join("skills/brainstorming")).unwrap();
        write_skill(
            &repo.path().join("skills/test-driven-development"),
            "test-driven-development",
            "Write tests first",
            "v2",
        );

        let refreshed = refresh_installed_skill_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(refreshed.id, "superpowers");
        assert_eq!(refreshed.skills.len(), 1);
        assert!(!roots
            .skills_root
            .join("superpowers--brainstorming")
            .exists());
        assert!(roots
            .skills_root
            .join("superpowers--test-driven-development")
            .join("SKILL.md")
            .is_file());
    }

    #[tokio::test]
    async fn refresh_plugin_pack_removes_old_pack_when_plugin_id_changes() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "superpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Superpowers" }
            })
            .to_string(),
        )
        .unwrap();
        write_skill(
            &repo.path().join("skills/brainstorming"),
            "brainstorming",
            "Explore ideas",
            "v1",
        );

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let installed = install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();
        assert_eq!(installed.id, "superpowers");
        assert!(roots.plugins_root.join("superpowers").exists());
        assert!(roots
            .skills_root
            .join("superpowers--brainstorming")
            .exists());

        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "mindpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Mindpowers" }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::remove_dir_all(repo.path().join("skills/brainstorming")).unwrap();
        write_skill(
            &repo.path().join("skills/focus"),
            "focus",
            "Stay on target",
            "v2",
        );

        let refreshed = refresh_installed_skill_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(refreshed.id, "mindpowers");
        assert_eq!(refreshed.plugin_id.as_deref(), Some("mindpowers"));
        assert!(!roots.plugins_root.join("superpowers").exists());
        assert!(!roots
            .skills_root
            .join("superpowers--brainstorming")
            .exists());
        assert!(roots.plugins_root.join("mindpowers").exists());
        assert!(roots.skills_root.join("mindpowers--focus").exists());

        let listed = list_installed_skills_in(&roots).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "mindpowers");
        assert_eq!(listed[0].skill_count, 1);
    }

    #[tokio::test]
    async fn refresh_single_skill_removes_old_id_when_skill_name_changes() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write_skill(repo.path(), "Brainstorming", "Explore ideas", "v1");

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/acme/skill-pack".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let installed = install_skill_source_with("acme/skill-pack", &roots, &cloner)
            .await
            .unwrap();
        assert_eq!(installed.id, "brainstorming");
        assert!(roots.skills_root.join("brainstorming").exists());

        write_skill(repo.path(), "Fresh Ideas", "Explore ideas", "v2");

        let refreshed = refresh_installed_skill_with("brainstorming", &roots, &cloner)
            .await
            .unwrap();
        assert_eq!(refreshed.id, "fresh-ideas");
        assert!(roots.skills_root.join("fresh-ideas").exists());
        assert!(!roots.skills_root.join("brainstorming").exists());

        let listed = list_installed_skills_in(&roots).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "fresh-ideas");
    }

    #[tokio::test]
    async fn refresh_plugin_pack_to_single_skill_removes_old_pack_artifacts() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "superpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Superpowers" }
            })
            .to_string(),
        )
        .unwrap();
        write_skill(
            &repo.path().join("skills/brainstorming"),
            "brainstorming",
            "Explore ideas",
            "v1",
        );

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let installed = install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();
        assert_eq!(installed.id, "superpowers");

        std::fs::remove_file(repo.path().join(".codex-plugin/plugin.json")).unwrap();
        std::fs::remove_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::remove_dir_all(repo.path().join("skills")).unwrap();
        write_skill(repo.path(), "Fresh Ideas", "Explore ideas", "v2");

        let refreshed = refresh_installed_skill_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(refreshed.id, "fresh-ideas");
        assert_eq!(refreshed.plugin_id, None);
        assert!(!roots.plugins_root.join("superpowers").exists());
        assert!(!roots
            .skills_root
            .join("superpowers--brainstorming")
            .exists());
        assert!(roots.skills_root.join("fresh-ideas").exists());

        let listed = list_installed_skills_in(&roots).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "fresh-ideas");
    }

    #[tokio::test]
    async fn refresh_plugin_pack_to_same_id_single_skill_removes_old_pack_artifacts() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "superpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Superpowers" }
            })
            .to_string(),
        )
        .unwrap();
        write_skill(
            &repo.path().join("skills/brainstorming"),
            "brainstorming",
            "Explore ideas",
            "v1",
        );

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let installed = install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();
        assert_eq!(installed.id, "superpowers");
        assert_eq!(installed.plugin_id.as_deref(), Some("superpowers"));
        assert!(roots.plugins_root.join("superpowers").exists());
        assert!(roots
            .skills_root
            .join("superpowers--brainstorming")
            .exists());

        std::fs::remove_file(repo.path().join(".codex-plugin/plugin.json")).unwrap();
        std::fs::remove_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::remove_dir_all(repo.path().join("skills")).unwrap();
        write_skill(repo.path(), "superpowers", "Explore ideas", "v2");

        let refreshed = refresh_installed_skill_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(refreshed.id, "superpowers");
        assert_eq!(refreshed.plugin_id, None);
        assert!(!roots.plugins_root.join("superpowers").exists());
        assert!(!roots
            .skills_root
            .join("superpowers--brainstorming")
            .exists());
        assert!(roots
            .skills_root
            .join("superpowers")
            .join("SKILL.md")
            .is_file());

        let listed = list_installed_skills_in(&roots).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "superpowers");
        assert_eq!(listed[0].plugin_id, None);
        assert_eq!(listed[0].skill_count, 1);
    }

    #[tokio::test]
    async fn refresh_single_skill_to_same_id_plugin_pack_removes_old_single_artifact() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write_skill(repo.path(), "superpowers", "Explore ideas", "v1");

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let installed = install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();
        assert_eq!(installed.id, "superpowers");
        assert_eq!(installed.plugin_id, None);
        assert!(roots.skills_root.join("superpowers").exists());

        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "superpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Superpowers" }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::remove_file(repo.path().join("SKILL.md")).unwrap();
        write_skill(
            &repo.path().join("skills/brainstorming"),
            "brainstorming",
            "Explore ideas",
            "v2",
        );

        let refreshed = refresh_installed_skill_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(refreshed.id, "superpowers");
        assert_eq!(refreshed.plugin_id.as_deref(), Some("superpowers"));
        assert!(!roots.skills_root.join("superpowers").exists());
        assert!(roots.plugins_root.join("superpowers").exists());
        assert!(roots
            .skills_root
            .join("superpowers--brainstorming")
            .join("SKILL.md")
            .is_file());

        let listed = list_installed_skills_in(&roots).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "superpowers");
        assert_eq!(listed[0].plugin_id.as_deref(), Some("superpowers"));
        assert_eq!(listed[0].skill_count, 1);
    }

    #[tokio::test]
    async fn remove_installed_skill_deletes_plugin_and_materialized_skills() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "superpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Superpowers" }
            })
            .to_string(),
        )
        .unwrap();
        write_skill(
            &repo.path().join("skills/brainstorming"),
            "brainstorming",
            "Explore ideas",
            "v1",
        );

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();
        remove_installed_skill_in(&roots, "superpowers").unwrap();

        assert!(!roots.plugins_root.join("superpowers").exists());
        assert!(!roots
            .skills_root
            .join("superpowers--brainstorming")
            .exists());
        assert!(list_installed_skills_in(&roots).unwrap().is_empty());
    }

    #[test]
    fn remove_installed_skill_deletes_hidden_mixed_generation_artifacts() {
        let config = tempfile::tempdir().unwrap();
        let roots = InstallRoots::new(config.path().to_path_buf());

        std::fs::create_dir_all(roots.plugins_root.join("superpowers")).unwrap();
        std::fs::write(
            roots
                .plugins_root
                .join("superpowers")
                .join("ryuzi-plugin.toml"),
            r#"
contract = 1
id = "superpowers"
name = "Superpowers"

[[skills]]
name = "focus"
description = "Stay on target"
path = "skills/focus"
"#
            .trim_start(),
        )
        .unwrap();
        write_installed_skill(
            &roots,
            "superpowers",
            "superpowers",
            "Stale single generation.",
            SkillInstallProvenance {
                source: "https://github.com/obra/superpowers".to_string(),
                plugin_id: None,
                installed_at: "2026-01-01T00:00:00.000Z".to_string(),
            },
        );
        write_installed_skill(
            &roots,
            "superpowers--focus",
            "focus",
            "Current pack generation.",
            SkillInstallProvenance {
                source: "https://github.com/obra/superpowers".to_string(),
                plugin_id: Some("superpowers".to_string()),
                installed_at: "2026-02-01T00:00:00.000Z".to_string(),
            },
        );

        let listed = list_installed_skills_in(&roots).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "superpowers");
        assert_eq!(listed[0].plugin_id.as_deref(), Some("superpowers"));

        remove_installed_skill_in(&roots, "superpowers").unwrap();

        assert!(!roots.plugins_root.join("superpowers").exists());
        assert!(!roots.skills_root.join("superpowers").exists());
        assert!(!roots.skills_root.join("superpowers--focus").exists());
        assert!(list_installed_skills_in(&roots).unwrap().is_empty());
    }

    #[tokio::test]
    async fn refresh_plugin_pack_plugin_id_change_deletes_hidden_old_identity_artifacts() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "superpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Superpowers" }
            })
            .to_string(),
        )
        .unwrap();
        write_skill(
            &repo.path().join("skills/brainstorming"),
            "brainstorming",
            "Explore ideas",
            "v1",
        );

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        write_installed_skill(
            &roots,
            "superpowers",
            "superpowers",
            "Hidden stale single generation.",
            SkillInstallProvenance {
                source: "https://github.com/obra/superpowers".to_string(),
                plugin_id: None,
                installed_at: "2025-12-31T23:59:59.999Z".to_string(),
            },
        );
        write_installed_skill(
            &roots,
            "superpowers--focus",
            "focus",
            "Hidden stale old-id pack skill.",
            SkillInstallProvenance {
                source: "https://github.com/obra/superpowers".to_string(),
                plugin_id: Some("superpowers".to_string()),
                installed_at: "2025-12-31T23:59:59.999Z".to_string(),
            },
        );

        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "mindpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Mindpowers" }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::remove_dir_all(repo.path().join("skills/brainstorming")).unwrap();
        write_skill(
            &repo.path().join("skills/focus"),
            "focus",
            "Stay on target",
            "v2",
        );

        let refreshed = refresh_installed_skill_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(refreshed.id, "mindpowers");
        assert!(!roots.plugins_root.join("superpowers").exists());
        assert!(!roots.skills_root.join("superpowers").exists());
        assert!(!roots
            .skills_root
            .join("superpowers--brainstorming")
            .exists());
        assert!(!roots.skills_root.join("superpowers--focus").exists());
        assert!(roots.plugins_root.join("mindpowers").exists());
        assert!(roots.skills_root.join("mindpowers--focus").exists());
    }

    #[tokio::test]
    async fn refresh_single_to_plugin_id_change_deletes_hidden_old_pack_artifacts() {
        let config = tempfile::tempdir().unwrap();
        let repo = tempfile::tempdir().unwrap();
        write_skill(repo.path(), "superpowers", "Explore ideas", "v1");

        let mut repos = BTreeMap::new();
        repos.insert(
            "https://github.com/obra/superpowers".to_string(),
            repo.path().to_path_buf(),
        );
        let roots = InstallRoots::new(config.path().to_path_buf());
        let cloner = FakeRepoCloner {
            repos,
            commit: None,
        };

        let installed = install_skill_source_with("superpowers", &roots, &cloner)
            .await
            .unwrap();
        assert_eq!(installed.id, "superpowers");
        assert_eq!(installed.plugin_id, None);
        assert!(roots.skills_root.join("superpowers").exists());

        std::fs::create_dir_all(roots.plugins_root.join("superpowers")).unwrap();
        std::fs::write(
            roots
                .plugins_root
                .join("superpowers")
                .join("ryuzi-plugin.toml"),
            r#"
contract = 1
id = "superpowers"
name = "Superpowers"

[[skills]]
name = "focus"
description = "Stay on target"
path = "skills/focus"
"#
            .trim_start(),
        )
        .unwrap();
        write_installed_skill(
            &roots,
            "superpowers--focus",
            "focus",
            "Hidden stale old-id pack skill.",
            SkillInstallProvenance {
                source: "https://github.com/obra/superpowers".to_string(),
                plugin_id: Some("superpowers".to_string()),
                installed_at: "2025-12-31T23:59:59.999Z".to_string(),
            },
        );

        std::fs::create_dir_all(repo.path().join(".codex-plugin")).unwrap();
        std::fs::write(
            repo.path().join(".codex-plugin/plugin.json"),
            serde_json::json!({
                "name": "mindpowers",
                "skills": "./skills/",
                "interface": { "displayName": "Mindpowers" }
            })
            .to_string(),
        )
        .unwrap();
        std::fs::remove_file(repo.path().join("SKILL.md")).unwrap();
        write_skill(
            &repo.path().join("skills/focus"),
            "focus",
            "Stay on target",
            "v2",
        );

        let refreshed = refresh_installed_skill_with("superpowers", &roots, &cloner)
            .await
            .unwrap();

        assert_eq!(refreshed.id, "mindpowers");
        assert_eq!(refreshed.plugin_id.as_deref(), Some("mindpowers"));
        assert!(!roots.plugins_root.join("superpowers").exists());
        assert!(!roots.skills_root.join("superpowers").exists());
        assert!(!roots.skills_root.join("superpowers--focus").exists());
        assert!(roots.plugins_root.join("mindpowers").exists());
        assert!(roots
            .skills_root
            .join("mindpowers--focus")
            .join("SKILL.md")
            .is_file());

        let listed = list_installed_skills_in(&roots).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, "mindpowers");
        assert_eq!(listed[0].plugin_id.as_deref(), Some("mindpowers"));
        assert_eq!(listed[0].skill_count, 1);
    }

    #[test]
    fn fingerprint_is_stable_and_excludes_git_and_stamp() {
        let a = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(a.path().join("skills/x")).unwrap();
        std::fs::write(a.path().join("skills/x/SKILL.md"), "hello").unwrap();
        let fp1 = fingerprint_dir(a.path()).unwrap();

        // Same content in a different dir → same fingerprint.
        let b = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(b.path().join("skills/x")).unwrap();
        std::fs::write(b.path().join("skills/x/SKILL.md"), "hello").unwrap();
        // Noise that must NOT affect the fingerprint:
        std::fs::create_dir_all(b.path().join(".git")).unwrap();
        std::fs::write(b.path().join(".git/config"), "junk").unwrap();
        std::fs::write(b.path().join(PROVENANCE_FILE), "{\"source\":\"x\"}").unwrap();
        let fp2 = fingerprint_dir(b.path()).unwrap();
        assert_eq!(fp1, fp2);

        // Changed content → different fingerprint.
        std::fs::write(b.path().join("skills/x/SKILL.md"), "changed").unwrap();
        assert_ne!(fp1, fingerprint_dir(b.path()).unwrap());
    }
}
