//! `skill` — load a skill's full instructions on demand (progressive
//! disclosure). Skills are discovered fresh from the worktree/global dirs on
//! each call via [`crate::harness::native::skills::SkillRegistry`].

use super::{truncate, PermissionSpec, Tool, ToolCtx, ToolOutput};
use crate::harness::native::skills::SkillRegistry;
use async_trait::async_trait;
use serde_json::{json, Value};

pub struct SkillTool;

#[async_trait]
impl Tool for SkillTool {
    fn name(&self) -> &str {
        "skill"
    }
    fn description(&self) -> &str {
        "Load the full instructions for a named skill. Skill names and \
         descriptions are listed in the system context; call this to read a \
         skill's body before performing its task."
    }
    fn input_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {"type": "string", "description": "The skill name to load."}
            },
            "required": ["name"]
        })
    }
    fn kind(&self) -> &'static str {
        "read"
    }
    fn permission(&self, input: &Value) -> PermissionSpec {
        let name = input.get("name").and_then(|v| v.as_str()).unwrap_or("");
        PermissionSpec::new("read", format!("load skill {name}"))
    }
    async fn execute(&self, ctx: &ToolCtx, input: Value) -> anyhow::Result<ToolOutput> {
        let name = input
            .get("name")
            .and_then(|v| v.as_str())
            .ok_or_else(|| anyhow::anyhow!("skill: `name` is required"))?;
        let reg = SkillRegistry::load(&ctx.work_dir);
        match reg.get(name) {
            Some(skill) => Ok(ToolOutput::ok(truncate(
                &format!("# Skill: {}\n\n{}", skill.name, skill.body),
                &ctx.caps,
            ))),
            None => Ok(ToolOutput::error(format!(
                "skill: no skill named `{name}` (available: {})",
                reg.names().join(", ")
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::testutil::ctx_at;
    use super::*;

    #[tokio::test]
    async fn loads_a_skill_body() {
        let dir = tempfile::tempdir().unwrap();
        let sd = dir.path().join(".ryuzi/skills/deploy");
        std::fs::create_dir_all(&sd).unwrap();
        std::fs::write(
            sd.join("SKILL.md"),
            "---\nname: deploy\ndescription: How to deploy\n---\nRun make deploy.",
        )
        .unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = SkillTool
            .execute(&ctx, json!({"name": "deploy"}))
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.for_model.contains("Run make deploy."));
    }

    #[tokio::test]
    async fn unknown_skill_errors() {
        let dir = tempfile::tempdir().unwrap();
        let ctx = ctx_at(dir.path()).await;
        let out = SkillTool
            .execute(&ctx, json!({"name": "nope"}))
            .await
            .unwrap();
        assert!(out.is_error);
    }
}
