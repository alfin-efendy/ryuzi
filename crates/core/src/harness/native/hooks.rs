//! Language-agnostic plugin hooks.
//!
//! Where opencode uses JS plugin modules, the native runtime uses external hook
//! scripts (git-hook style) so plugins can be written in any language. Scripts
//! live in `.ryuzi/hooks/<event>/` and receive the event payload as JSON on
//! stdin. For a gating event (`tool.before`), a non-zero exit denies the action
//! and the script's stdout becomes the reason. Observational events
//! (`tool.after`, `session.start`) ignore the result.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use tokio::io::AsyncWriteExt;

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
/// For gating events, the first non-zero exit denies and returns its stdout as
/// the message. Missing hook dir / spawn failures are treated as allow.
pub async fn run(work_dir: &Path, event: &str, payload: &Value) -> HookResult {
    let input = serde_json::to_vec(payload).unwrap_or_default();
    for script in hook_scripts(work_dir, event) {
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
        if !out.status.success() {
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
        let r = run(dir.path(), "tool.before", &json!({})).await;
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
        let r = run(dir.path(), "tool.before", &json!({"tool": "bash"})).await;
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
        let r = run(dir.path(), "tool.before", &json!({"tool": "read"})).await;
        assert!(r.allowed);
    }
}
