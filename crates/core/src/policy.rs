use crate::domain::PermMode;

const SAFE_TOOLS: &[&str] = &["Read", "Grep", "Glob", "LS", "NotebookRead", "TodoWrite"];
const EDIT_TOOLS: &[&str] = &["Edit", "Write", "MultiEdit", "NotebookEdit"];

/// `true` = auto-allow without asking the user.
pub fn resolve_tool_policy(perm: PermMode, tool: &str) -> bool {
    if perm == PermMode::BypassPermissions {
        return true;
    }
    if SAFE_TOOLS.contains(&tool) {
        return true;
    }
    if perm == PermMode::AcceptEdits && EDIT_TOOLS.contains(&tool) {
        return true;
    }
    false
}

/// Outcome of a combined policy decision.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PolicyOutcome {
    /// Auto-allow: no prompt needed.
    AutoAllow,
    /// Requires a user prompt.
    Prompt,
}

/// Combined policy decision: per-project allowAlways policy takes precedence,
/// then the mode-based auto-allow, otherwise Prompt.
///
/// `project_policy` is the raw decision string returned by
/// `Store::get_tool_policy` — currently only `"allowAlways"` triggers an
/// `AutoAllow`; all other values (including `None`) fall through.
pub fn decide_tool_permission(
    perm_mode: PermMode,
    project_policy: Option<&str>,
    tool: &str,
) -> PolicyOutcome {
    if project_policy == Some("allowAlways") {
        return PolicyOutcome::AutoAllow;
    }
    if resolve_tool_policy(perm_mode, tool) {
        return PolicyOutcome::AutoAllow;
    }
    PolicyOutcome::Prompt
}

pub fn tool_summary(name: &str, input: &serde_json::Value) -> String {
    if name == "Bash" {
        if let Some(cmd) = input.get("command").and_then(|c| c.as_str()) {
            return format!("Bash: {}", cmd.chars().take(80).collect::<String>());
        }
    }
    for key in ["file_path", "path", "pattern"] {
        if let Some(t) = input.get(key).and_then(|v| v.as_str()) {
            return format!("{name}: {t}");
        }
    }
    name.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::PermMode;
    use serde_json::json;

    #[test]
    fn safe_tools_auto_allow_in_default_mode() {
        assert!(resolve_tool_policy(PermMode::Default, "Read"));
        assert!(!resolve_tool_policy(PermMode::Default, "Bash"));
        assert!(resolve_tool_policy(PermMode::AcceptEdits, "Edit"));
        assert!(resolve_tool_policy(PermMode::BypassPermissions, "Bash"));
    }

    #[test]
    fn decide_tool_permission_allow_always_policy_overrides_all() {
        // allowAlways project policy → AutoAllow even for Bash in Default mode
        assert_eq!(
            decide_tool_permission(PermMode::Default, Some("allowAlways"), "Bash"),
            PolicyOutcome::AutoAllow
        );
    }

    #[test]
    fn decide_tool_permission_mode_based_auto_allow() {
        // No project policy, but mode auto-allows Read
        assert_eq!(
            decide_tool_permission(PermMode::Default, None, "Read"),
            PolicyOutcome::AutoAllow
        );
        // BypassPermissions auto-allows Bash
        assert_eq!(
            decide_tool_permission(PermMode::BypassPermissions, None, "Bash"),
            PolicyOutcome::AutoAllow
        );
        // AcceptEdits auto-allows Edit
        assert_eq!(
            decide_tool_permission(PermMode::AcceptEdits, None, "Edit"),
            PolicyOutcome::AutoAllow
        );
    }

    #[test]
    fn decide_tool_permission_else_prompt() {
        // No project policy, Default mode, Bash → Prompt
        assert_eq!(
            decide_tool_permission(PermMode::Default, None, "Bash"),
            PolicyOutcome::Prompt
        );
        // Unknown policy value → Prompt
        assert_eq!(
            decide_tool_permission(PermMode::Default, Some("rejectAlways"), "Read"),
            // Read is in SAFE_TOOLS → AutoAllow regardless of project_policy
            PolicyOutcome::AutoAllow
        );
        // Unknown policy + unknown tool → Prompt
        assert_eq!(
            decide_tool_permission(PermMode::Default, Some("rejectAlways"), "Bash"),
            PolicyOutcome::Prompt
        );
    }

    #[test]
    fn bash_summary_truncates_to_80_chars() {
        let long = "a".repeat(100);
        let out = tool_summary("Bash", &json!({ "command": long }));
        assert_eq!(out, format!("Bash: {}", "a".repeat(80)));
    }

    #[test]
    fn tool_summary_formats_bash_and_paths() {
        assert_eq!(
            tool_summary("Bash", &json!({"command": "ls -la"})),
            "Bash: ls -la"
        );
        assert_eq!(
            tool_summary("Read", &json!({"file_path": "/a/b.rs"})),
            "Read: /a/b.rs"
        );
        assert_eq!(tool_summary("Weird", &json!({})), "Weird");
    }
}
