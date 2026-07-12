//! `skill` — load a skill's full instructions on demand (progressive
//! disclosure). Skills are discovered fresh from the worktree/global dirs
//! (plus any plugin-bundled skill dirs) on each call via
//! [`crate::harness::native::skills::SkillRegistry`].

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
        let reg = SkillRegistry::load_with(&ctx.work_dir, &ctx.extra_skill_dirs);
        match reg.get(name) {
            Some(skill) => {
                // Record the view BEFORE returning: `ctx.viewed_skills` lets a
                // same-turn `skill_manage` call tell "viewed-then-used" apart
                // from "used blind" (the background-review guard, Task 6);
                // `skill_usage.view_count` feeds the curator's (Task 10)
                // usage heuristics. Best-effort — a store hiccup must not
                // block the read the model actually asked for.
                ctx.viewed_skills.lock().await.insert(skill.name.clone());
                let _ = ctx.store.record_skill_view(&skill.name).await;
                Ok(ToolOutput::ok(truncate(
                    &format!("# Skill: {}\n\n{}", skill.name, skill.body),
                    &ctx.caps,
                )))
            }
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
    async fn loading_a_skill_records_the_view_in_ctx_and_skill_usage() {
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
        assert!(ctx.viewed_skills.lock().await.contains("deploy"));
        let usage = ctx.store.get_skill_usage("deploy").await.unwrap().unwrap();
        assert_eq!(usage.view_count, 1);
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
