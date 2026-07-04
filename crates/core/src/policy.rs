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

/// Whether a clicker may approve a tool. The session starter always may. If
/// NO approver roles are configured, only the starter may approve
/// (safe-by-default). Otherwise the clicker must hold one of the approver
/// roles.
pub fn can_approve(
    clicker_role_ids: &[String],
    approver_role_ids: &[String],
    is_starter: bool,
) -> bool {
    if is_starter {
        return true;
    }
    if approver_role_ids.is_empty() {
        return false;
    }
    clicker_role_ids
        .iter()
        .any(|r| approver_role_ids.contains(r))
}

/// Whether a user holds an admin role. If NO admin roles are configured,
/// everyone is treated as admin — opposite default from `can_approve`, and
/// preserves the zero-config single-user UX.
pub fn is_admin(user_role_ids: &[String], admin_role_ids: &[String]) -> bool {
    if admin_role_ids.is_empty() {
        return true;
    }
    user_role_ids.iter().any(|r| admin_role_ids.contains(r))
}

/// Clamp a privileged permission mode for non-admins. Only `BypassPermissions`
/// is gated (it disables all tool approval). Returns the effective mode and
/// whether it was downgraded, so the caller can warn the user.
pub fn gate_perm_mode(requested: PermMode, is_admin_user: bool) -> (PermMode, bool) {
    if !is_admin_user && requested == PermMode::BypassPermissions {
        return (PermMode::Default, true);
    }
    (requested, false)
}

/// Split a comma-separated role-id setting into a trimmed, non-empty list.
/// Identical semantics to [`crate::settings::csv`], to which it delegates.
pub fn parse_role_ids(raw: Option<&str>) -> Vec<String> {
    crate::settings::csv(raw)
}

/// Summarize a tool call for display: `Bash` truncates its command to 80
/// chars, others fall back to a `file_path`/`path`/`pattern` string input, or
/// the bare tool name. Delegates to [`tool_summary`] above, which implements
/// identical behavior (kept as a distinct public name for callers that know
/// the policy surface by this name).
pub fn summarize_tool(tool_name: &str, input: &serde_json::Value) -> String {
    tool_summary(tool_name, input)
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

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn can_approve_starter_always_wins() {
        assert!(can_approve(&ids(&[]), &ids(&[]), true));
    }

    #[test]
    fn can_approve_empty_approver_list_means_starter_only() {
        assert!(!can_approve(&ids(&[]), &ids(&[]), false));
    }

    #[test]
    fn can_approve_role_intersection() {
        assert!(can_approve(&ids(&["r1"]), &ids(&["r1"]), false));
        assert!(!can_approve(&ids(&["r2"]), &ids(&["r1"]), false));
    }

    #[test]
    fn is_admin_empty_admin_list_means_everyone_is_admin() {
        assert!(is_admin(&ids(&[]), &ids(&[])));
        assert!(is_admin(&ids(&["x"]), &ids(&[])));
    }

    #[test]
    fn is_admin_configured_roles_gate_membership() {
        assert!(is_admin(&ids(&["admin"]), &ids(&["admin"])));
        assert!(!is_admin(&ids(&["other"]), &ids(&["admin"])));
        assert!(!is_admin(&ids(&[]), &ids(&["admin"])));
    }

    #[test]
    fn gate_perm_mode_clamps_only_bypass_permissions_for_non_admins() {
        assert_eq!(
            gate_perm_mode(PermMode::BypassPermissions, false),
            (PermMode::Default, true)
        );
        assert_eq!(
            gate_perm_mode(PermMode::BypassPermissions, true),
            (PermMode::BypassPermissions, false)
        );
        assert_eq!(
            gate_perm_mode(PermMode::AcceptEdits, false),
            (PermMode::AcceptEdits, false)
        );
        assert_eq!(
            gate_perm_mode(PermMode::Default, false),
            (PermMode::Default, false)
        );
    }

    #[test]
    fn parse_role_ids_splits_trims_and_drops_empties() {
        assert_eq!(parse_role_ids(Some("a, b ,,c")), vec!["a", "b", "c"]);
        assert!(parse_role_ids(Some("")).is_empty());
        assert!(parse_role_ids(None).is_empty());
    }

    #[test]
    fn summarize_tool_bash_slices_command_to_80_chars() {
        let long = "a".repeat(100);
        assert_eq!(
            summarize_tool("Bash", &json!({ "command": long })),
            format!("Bash: {}", "a".repeat(80))
        );
    }

    #[test]
    fn summarize_tool_formats_bash_and_paths_or_bare_name() {
        assert_eq!(
            summarize_tool("Bash", &json!({"command": "echo hi"})),
            "Bash: echo hi"
        );
        assert_eq!(
            summarize_tool("Edit", &json!({"file_path": "src/a.ts"})),
            "Edit: src/a.ts"
        );
        assert_eq!(summarize_tool("Glob", &json!({})), "Glob");
    }
}
