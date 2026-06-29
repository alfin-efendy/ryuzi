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
    fn bash_summary_truncates_to_80_chars() {
        let long = "a".repeat(100);
        let out = tool_summary("Bash", &json!({ "command": long }));
        assert_eq!(out, format!("Bash: {}", "a".repeat(80)));
    }

    #[test]
    fn tool_summary_formats_bash_and_paths() {
        assert_eq!(tool_summary("Bash", &json!({"command": "ls -la"})), "Bash: ls -la");
        assert_eq!(tool_summary("Read", &json!({"file_path": "/a/b.rs"})), "Read: /a/b.rs");
        assert_eq!(tool_summary("Weird", &json!({})), "Weird");
    }
}
