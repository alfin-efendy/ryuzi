//! Language-agnostic plugin hooks.
//!
//! Where opencode uses JS plugin modules, the native runtime uses external hook
//! scripts (git-hook style) so plugins can be written in any language. Scripts
//! live in `.ryuzi/hooks/<event>/` and receive the event payload as JSON on
//! stdin. For a gating event (`tool.before`), a non-zero exit denies the action
//! and the script's stdout becomes the reason. Observational events
//! (`session.start`, `tool.after`, `session.end`) ignore the result.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

/// The typed vocabulary of hook events the native runtime dispatches. This is
/// also what Track D's subprocess extension host subscribes to, so the string
/// form (`as_str`) is a stable wire contract, not an implementation detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HookEvent {
    SessionStart,
    ToolBefore,
    ToolAfter,
    SessionEnd,
}

impl HookEvent {
    /// The `.ryuzi/hooks/<event>/` directory name / wire identifier.
    pub fn as_str(&self) -> &'static str {
        match self {
            HookEvent::SessionStart => "session.start",
            HookEvent::ToolBefore => "tool.before",
            HookEvent::ToolAfter => "tool.after",
            HookEvent::SessionEnd => "session.end",
        }
    }

    /// Only `tool.before` can deny an action; every other event is
    /// fire-and-forget observation.
    pub fn is_gating(&self) -> bool {
        matches!(self, HookEvent::ToolBefore)
    }

    pub const ALL: &'static [HookEvent] = &[
        HookEvent::SessionStart,
        HookEvent::ToolBefore,
        HookEvent::ToolAfter,
        HookEvent::SessionEnd,
    ];
}

/// The outcome of running an event's hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookResult {
    pub allowed: bool,
    pub message: Option<String>,
}

impl HookResult {
    fn allow() -> Self {
        HookResult {
            allowed: true,
            message: None,
        }
    }
}

fn hook_scripts(work_dir: &Path, event: &str) -> Vec<PathBuf> {
    let dir = work_dir.join(".ryuzi/hooks").join(event);
    let Ok(entries) = std::fs::read_dir(&dir) else {
        return vec![];
    };
    let mut scripts: Vec<PathBuf> = entries
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.is_file())
        .collect();
    scripts.sort();
    scripts
}

/// Run all hooks registered for `event`, feeding `payload` as JSON on stdin.
/// For a gating event, the first non-zero exit denies and returns its stdout
/// as the message. For an observational event, a non-zero exit is ignored
/// (the remaining scripts still run) — it can never deny. Missing hook dir /
/// spawn failures are treated as allow.
pub async fn run(work_dir: &Path, event: HookEvent, payload: &Value) -> HookResult {
    let input = serde_json::to_vec(payload).unwrap_or_default();
    for script in hook_scripts(work_dir, event.as_str()) {
        let mut cmd = tokio::process::Command::new(&script);
        cmd.current_dir(work_dir)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null());
        crate::process_util::no_window(&mut cmd);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(_) => continue, // not executable / not runnable — skip
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(&input).await;
            drop(stdin); // close so the hook sees EOF
        }
        let Ok(out) = child.wait_with_output().await else {
            continue;
        };
        if !out.status.success() && event.is_gating() {
            let msg = String::from_utf8_lossy(&out.stdout).trim().to_string();
            return HookResult {
                allowed: false,
                message: Some(if msg.is_empty() {
                    format!("blocked by hook {}", script.display())
                } else {
                    msg
                }),
            };
        }
    }
    HookResult::allow()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[cfg(unix)]
    fn write_hook(dir: &Path, event: &str, name: &str, body: &str) {
        use std::os::unix::fs::PermissionsExt;
        let hook_dir = dir.join(".ryuzi/hooks").join(event);
        std::fs::create_dir_all(&hook_dir).unwrap();
        let path = hook_dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
    }

    #[tokio::test]
    async fn no_hooks_dir_allows() {
        let dir = tempfile::tempdir().unwrap();
        let r = run(dir.path(), HookEvent::ToolBefore, &json!({})).await;
        assert_eq!(r, HookResult::allow());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn a_denying_hook_blocks_with_its_message() {
        let dir = tempfile::tempdir().unwrap();
        write_hook(
            dir.path(),
            "tool.before",
            "deny.sh",
            "#!/bin/sh\necho 'bash is not allowed here'\nexit 1\n",
        );
        let r = run(dir.path(), HookEvent::ToolBefore, &json!({"tool": "bash"})).await;
        assert!(!r.allowed);
        assert_eq!(r.message.as_deref(), Some("bash is not allowed here"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn an_allowing_hook_permits() {
        let dir = tempfile::tempdir().unwrap();
        write_hook(
            dir.path(),
            "tool.before",
            "ok.sh",
            "#!/bin/sh\ncat >/dev/null\nexit 0\n",
        );
        let r = run(dir.path(), HookEvent::ToolBefore, &json!({"tool": "read"})).await;
        assert!(r.allowed);
    }

    /// The gating/observational split: `tool.before` is the only event that
    /// can deny. A `tool.after` hook that exits non-zero (and even tries to
    /// pass a "denial" message on stdout) must never flip `allowed` — its
    /// result is ignored, matching the module doc's fire-and-forget contract.
    #[cfg(unix)]
    #[tokio::test]
    async fn an_observational_denying_hook_does_not_deny() {
        let dir = tempfile::tempdir().unwrap();
        write_hook(
            dir.path(),
            "tool.after",
            "loud.sh",
            "#!/bin/sh\ncat >/dev/null\necho 'i tried to block this'\nexit 1\n",
        );
        let r = run(dir.path(), HookEvent::ToolAfter, &json!({"tool": "bash"})).await;
        assert!(
            r.allowed,
            "observational hook must never deny: {:?}",
            r.message
        );
    }

    #[test]
    fn hook_event_as_str_matches_the_wire_vocabulary() {
        assert_eq!(HookEvent::SessionStart.as_str(), "session.start");
        assert_eq!(HookEvent::ToolBefore.as_str(), "tool.before");
        assert_eq!(HookEvent::ToolAfter.as_str(), "tool.after");
        assert_eq!(HookEvent::SessionEnd.as_str(), "session.end");
    }

    // Guards the cross-crate contract: the SDK validates `[[extension]]`
    // `events` against `ryuzi_plugin_sdk::KNOWN_HOOK_EVENTS`, but the events
    // are actually fired here by `HookEvent`. If the two drift (a renamed
    // variant, a new event on one side only), a manifest's subscription is
    // accepted at parse time but never delivered at runtime — a silent break.
    // This test fails to compile-or-assert the moment they diverge.
    #[test]
    fn hook_event_vocabulary_matches_the_sdk_known_events() {
        let core: Vec<&str> = HookEvent::ALL.iter().map(|e| e.as_str()).collect();
        assert_eq!(
            core,
            ryuzi_plugin_sdk::KNOWN_HOOK_EVENTS,
            "core HookEvent vocabulary must stay in sync with the SDK's KNOWN_HOOK_EVENTS"
        );
    }

    #[test]
    fn only_tool_before_is_gating() {
        assert!(!HookEvent::SessionStart.is_gating());
        assert!(HookEvent::ToolBefore.is_gating());
        assert!(!HookEvent::ToolAfter.is_gating());
        assert!(!HookEvent::SessionEnd.is_gating());
    }
}
