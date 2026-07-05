//! Skills: progressive-disclosure capability docs, mirroring opencode/Claude
//! skills. A skill is a `SKILL.md` (name + description frontmatter, markdown
//! body) under `.ryuzi/skills/<name>/`, `~/.config/ryuzi/skills/<name>/`, or
//! `~/.claude/skills/<name>/`. Only names+descriptions are surfaced to the
//! model up front; the full body is fetched on demand via the `skill` tool.

use std::collections::BTreeMap;
use std::path::Path;

/// One discovered skill.
#[derive(Debug, Clone)]
pub struct Skill {
    pub name: String,
    pub description: String,
    pub body: String,
}

/// The set of available skills.
pub struct SkillRegistry {
    skills: BTreeMap<String, Skill>,
}

impl SkillRegistry {
    /// Discover skills under the worktree and the global config/claude dirs.
    pub fn load(work_dir: &Path) -> SkillRegistry {
        let mut skills = BTreeMap::new();
        for base in skill_dirs(work_dir) {
            for skill in read_skills(&base) {
                skills.entry(skill.name.clone()).or_insert(skill);
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

    /// A `- name: description` list for the system prompt, or `None` if empty.
    pub fn guidance(&self) -> Option<String> {
        if self.skills.is_empty() {
            return None;
        }
        let list = self
            .skills
            .values()
            .map(|s| format!("- {}: {}", s.name, s.description))
            .collect::<Vec<_>>()
            .join("\n");
        Some(format!(
            "Available skills (load a skill's full instructions with the `skill` tool before using it):\n{list}"
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
            let dir_name = e.file_name().to_string_lossy().to_string();
            let text = std::fs::read_to_string(e.path().join("SKILL.md")).ok()?;
            Some(parse_skill(&dir_name, &text))
        })
        .collect()
}

fn parse_skill(dir_name: &str, text: &str) -> Skill {
    let (frontmatter, body) = super::agents::split_frontmatter_pub(text);
    let mut name = dir_name.to_string();
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
        assert!(reg.guidance().unwrap().contains("pdf: Work with PDFs"));
    }

    #[test]
    fn empty_skills_dir_yields_nothing() {
        // read_skills over a non-existent / empty dir returns no skills.
        let dir = tempfile::tempdir().unwrap();
        assert!(read_skills(&dir.path().join(".ryuzi/skills")).is_empty());
    }

    #[test]
    fn parse_skill_falls_back_to_dir_name() {
        let s = parse_skill("mytool", "No frontmatter, just a body.");
        assert_eq!(s.name, "mytool");
        assert!(s.description.contains("mytool"));
        assert_eq!(s.body, "No frontmatter, just a body.");
    }
}
