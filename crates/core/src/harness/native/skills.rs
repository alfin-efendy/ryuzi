//! Skills: progressive-disclosure capability docs, mirroring opencode/Claude
//! skills. A skill is a `SKILL.md` (name + description frontmatter, markdown
//! body) under `.ryuzi/skills/<name>/`, `~/.config/ryuzi/skills/<name>/`, or
//! `~/.claude/skills/<name>/`. Only names+descriptions are surfaced to the
//! model up front; the full body is fetched on demand via the `skill` tool.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// One discovered skill.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
    /// The leaf directory this skill was discovered in (i.e. the directory
    /// containing its `SKILL.md`), used to resolve companion files the skill
    /// ships alongside its instructions.
    pub dir: PathBuf,
}

/// The set of available skills.
pub struct SkillRegistry {
    skills: BTreeMap<String, Skill>,
}

impl SkillRegistry {
    /// Discover skills under the worktree and the global config/claude dirs.
    pub fn load(work_dir: &Path) -> SkillRegistry {
        Self::load_with(work_dir, &[])
    }

    /// Like [`Self::load`], but also scans `extra` directories — each
    /// can be either:
    ///   - A skills root (i.e. `<extra>/<name>/SKILL.md`), exactly like
    ///     `.ryuzi/skills` or `~/.claude/skills`, OR
    ///   - A leaf skill directory (i.e. `<extra>/SKILL.md` directly).
    ///
    /// Used to fold in plugin-bundled skill directories
    /// (`PluginHost::enabled_skill_dirs`) beside the worktree/global ones.
    /// A name already found in an earlier (worktree/global) directory wins
    /// over one from `extra`.
    pub fn load_with(work_dir: &Path, extra: &[std::path::PathBuf]) -> SkillRegistry {
        let mut skills = BTreeMap::new();
        for base in skill_dirs(work_dir).iter().chain(extra) {
            // Check if this is a leaf skill dir (SKILL.md at the base).
            if base.join("SKILL.md").is_file() {
                // This is a leaf: parse it as a single skill.
                if let Ok(text) = std::fs::read_to_string(base.join("SKILL.md")) {
                    let skill = parse_skill(base, &text);
                    skills.entry(skill.name.clone()).or_insert(skill);
                }
            } else {
                // This is a root: scan for subdirectories containing SKILL.md.
                for skill in read_skills(base) {
                    skills.entry(skill.name.clone()).or_insert(skill);
                }
            }
        }
        SkillRegistry { skills }
    }

    pub fn get(&self, name: &str) -> Option<Skill> {
        self.skills.get(name).cloned()
    }

    pub fn all(&self) -> Vec<Skill> {
        self.skills.values().cloned().collect()
    }

    pub fn names(&self) -> Vec<String> {
        self.skills.keys().cloned().collect()
    }

    /// A `- name: description` list for the system prompt, or `None` if
    /// empty. Descriptions are truncated to 60 chars — the index is a
    /// scan-and-decide surface, not the skill's full documentation (that's
    /// what the `skill` tool loads on demand).
    pub fn guidance(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let list = self
            .skills
            .values()
            .map(|s| {
                let d: String = s.description.chars().take(60).collect();
                format!("- {}: {d}", s.name)
            })
            .collect::<Vec<_>>()
            .join("\n");
        Some(format!(
            "Available skills. You MUST scan this list at the start of every \
             task and load a skill's full instructions with the `skill` tool \
             BEFORE doing work it covers.\n{list}"
        ))
    }
}

fn skill_dirs(work_dir: &Path) -> Vec<std::path::PathBuf> {
    let mut dirs = vec![work_dir.join(".ryuzi/skills")];
    if let Some(home) = dirs::home_dir() {
        dirs.push(home.join(".config/ryuzi/skills"));
        dirs.push(home.join(".claude/skills"));
    }
    dirs
}

/// Read `<base>/<name>/SKILL.md` skills from a skills directory.
fn read_skills(base: &Path) -> Vec<Skill> {
    let Ok(entries) = std::fs::read_dir(base) else {
        return vec![];
    };
    entries
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .filter_map(|e| {
            let path = e.path();
            let text = std::fs::read_to_string(path.join("SKILL.md")).ok()?;
            Some(parse_skill(&path, &text))
        })
        .collect()
}

fn parse_skill(dir: &Path, text: &str) -> Skill {
    let (frontmatter, body) = super::agents::split_frontmatter_pub(text);
    let dir_name = dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();
    let mut name = dir_name.clone();
    let mut description = format!("Skill `{dir_name}`");
    for (key, value) in frontmatter {
        match key.as_str() {
            "name" => name = value,
            "description" => description = value,
            _ => {}
        }
    }
    Skill {
        name,
        description,
        body: body.trim().to_string(),
        dir: dir.to_path_buf(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discovers_skill_with_frontmatter() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".ryuzi/skills/pdf");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: pdf\ndescription: Work with PDFs\n---\nUse pdftotext to extract text.",
        )
        .unwrap();
        let reg = SkillRegistry::load(dir.path());
        let s = reg.get("pdf").unwrap();
        assert_eq!(s.description, "Work with PDFs");
        assert!(s.body.contains("pdftotext"));
        assert_eq!(s.dir, skill_dir);
        assert!(reg.guidance().unwrap().contains("pdf: Work with PDFs"));
    }

    #[test]
    fn load_with_merges_an_extra_dir_alongside_the_worktree_ones() {
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".ryuzi/skills/pdf");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: pdf\ndescription: Work with PDFs\n---\nUse pdftotext to extract text.",
        )
        .unwrap();

        // A plugin-bundled skills root, entirely outside the worktree.
        let extra = tempfile::tempdir().unwrap();
        let extra_skill_dir = extra.path().join("triage");
        std::fs::create_dir_all(&extra_skill_dir).unwrap();
        std::fs::write(
            extra_skill_dir.join("SKILL.md"),
            "---\nname: triage\ndescription: Triage issues\n---\nLabel and assign.",
        )
        .unwrap();

        let reg = SkillRegistry::load_with(dir.path(), &[extra.path().to_path_buf()]);
        assert_eq!(reg.get("pdf").unwrap().description, "Work with PDFs");
        let s = reg.get("triage").unwrap();
        assert_eq!(s.description, "Triage issues");
        assert!(s.body.contains("Label and assign."));
        // Check that both skills are present (there may be other global skills too).
        let names = reg.names();
        assert!(
            names.contains(&"pdf".to_string()),
            "pdf skill must be present"
        );
        assert!(
            names.contains(&"triage".to_string()),
            "triage skill must be present"
        );
    }

    #[test]
    fn load_with_no_extra_dirs_matches_load() {
        let dir = tempfile::tempdir().unwrap();
        // Both load and load_with should have the same result when no extras are
        // provided (but may include global skills from ~/.claude/skills, etc).
        let via_load = SkillRegistry::load(dir.path()).names();
        let via_load_with = SkillRegistry::load_with(dir.path(), &[]).names();
        assert_eq!(via_load, via_load_with);
    }

    #[test]
    fn empty_skills_dir_yields_nothing() {
        // read_skills over a non-existent / empty dir returns no skills.
        let dir = tempfile::tempdir().unwrap();
        assert!(read_skills(&dir.path().join(".ryuzi/skills")).is_empty());
    }

    #[test]
    fn guidance_truncates_descriptions_to_60_chars_and_demands_a_scan() {
        let mut skills = std::collections::BTreeMap::new();
        skills.insert(
            "x".into(),
            Skill {
                name: "x".into(),
                description: "a".repeat(200),
                body: String::new(),
                dir: std::path::PathBuf::new(),
            },
        );
        let g = SkillRegistry { skills }.guidance().unwrap();
        assert!(
            g.contains("You MUST scan"),
            "mandatory-scan wording missing: {g}"
        );
        assert!(
            !g.contains(&"a".repeat(61)),
            "description not truncated to 60 chars"
        );
    }

    #[test]
    fn parse_skill_falls_back_to_dir_name() {
        let s = parse_skill(Path::new("mytool"), "No frontmatter, just a body.");
        assert_eq!(s.name, "mytool");
        assert!(s.description.contains("mytool"));
        assert_eq!(s.body, "No frontmatter, just a body.");
        assert_eq!(s.dir, Path::new("mytool"));
    }

    #[test]
    fn load_with_extra_leaf_dir_containing_skill_md() {
        // Test that plugin-bundled leaf skill dirs (SKILL.md directly inside)
        // are discovered as single skills, not roots.
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".ryuzi/skills/pdf");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: pdf\ndescription: Work with PDFs\n---\nUse pdftotext to extract text.",
        )
        .unwrap();

        // A plugin-bundled LEAF skill dir (not a root).
        let plugin_dir = tempfile::tempdir().unwrap();
        let plugin_skill = plugin_dir.path().join("github-triage");
        std::fs::create_dir_all(&plugin_skill).unwrap();
        std::fs::write(
            plugin_skill.join("SKILL.md"),
            "---\nname: github-triage\ndescription: Triage GitHub issues\n---\nLabel and assign issues.",
        )
        .unwrap();

        let reg = SkillRegistry::load_with(dir.path(), std::slice::from_ref(&plugin_skill));
        assert_eq!(reg.get("pdf").unwrap().description, "Work with PDFs");
        let s = reg.get("github-triage").unwrap();
        assert_eq!(s.description, "Triage GitHub issues");
        assert!(s.body.contains("Label and assign"));
        assert_eq!(s.dir, plugin_skill);
        // Check that both skills are present (there may be other global skills too).
        let names = reg.names();
        assert!(
            names.contains(&"pdf".to_string()),
            "pdf skill must be present"
        );
        assert!(
            names.contains(&"github-triage".to_string()),
            "github-triage skill must be present"
        );
    }

    #[test]
    fn load_with_extra_root_dir_with_subdirs() {
        // Test that root-shaped extra dirs (with subdirectories containing
        // SKILL.md) still work as before.
        let dir = tempfile::tempdir().unwrap();
        let skill_dir = dir.path().join(".ryuzi/skills/pdf");
        std::fs::create_dir_all(&skill_dir).unwrap();
        std::fs::write(
            skill_dir.join("SKILL.md"),
            "---\nname: pdf\ndescription: Work with PDFs\n---\nUse pdftotext to extract text.",
        )
        .unwrap();

        // A plugin-bundled ROOT dir with subdirectories.
        let plugin_root = tempfile::tempdir().unwrap();
        let plugin_skill = plugin_root.path().join("github-triage");
        std::fs::create_dir_all(&plugin_skill).unwrap();
        std::fs::write(
            plugin_skill.join("SKILL.md"),
            "---\nname: github-triage\ndescription: Triage GitHub issues\n---\nLabel and assign issues.",
        )
        .unwrap();

        let reg = SkillRegistry::load_with(dir.path(), &[plugin_root.path().to_path_buf()]);
        assert_eq!(reg.get("pdf").unwrap().description, "Work with PDFs");
        let s = reg.get("github-triage").unwrap();
        assert_eq!(s.description, "Triage GitHub issues");
        assert!(s.body.contains("Label and assign"));
        assert_eq!(s.dir, plugin_skill);
    }
}
