//! Language-agnostic plugin hooks.
//!
//! Where opencode uses JS plugin modules, the native runtime uses external hook
//! scripts (git-hook style) so plugins can be written in any language. Scripts
//! live in `.ryuzi/hooks/<event>/` and receive the event payload as JSON on
//! stdin. For a gating event (`tool.before`), a non-zero exit denies the action
//! and the script's stdout becomes the reason. Observational events
//! (`session.start`, `tool.after`, `session.end`) ignore the result.
//!
//! [`run`] is Track C's ONE sink: on-disk scripts. Track D
//! (`crate::plugins::extension`) generalizes the same typed [`HookEvent`]
//! vocabulary to a SECOND sink — supervised extension subprocesses — via
//! [`fire_hook`], which runs both sinks and combines their results (either
//! denying denies, for a gating event). Every call site in `harness::native`
//! fires through [`fire_hook`], not `run` directly, so a session with no
//! extensions registered (`SessionCtx.extension_events: None`, the common
//! case) behaves exactly as it did before Track D existed.

use serde_json::Value;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use tokio::io::AsyncWriteExt;

use crate::plugins::extension::ExtensionEvents;

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

impl std::str::FromStr for HookEvent {
    type Err = String;

    /// The inverse of [`HookEvent::as_str`] — used by Track D's declarative
    /// extension binding (`plugins::extension`) to turn a manifest's
    /// already-`validate()`d `events: Vec<String>` into typed `HookEvent`s.
    /// `PluginManifest::validate` already rejects any string outside
    /// `ryuzi_plugin_sdk::KNOWN_HOOK_EVENTS` (kept in sync with `as_str` by
    /// `hook_event_vocabulary_matches_the_sdk_known_events`, below), so this
    /// only errors on a value that bypassed validation (e.g. a hand-built
    /// `ExtensionDef` in a test).
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        HookEvent::ALL
            .iter()
            .find(|event| event.as_str() == s)
            .copied()
            .ok_or_else(|| format!("unknown hook event: {s}"))
    }
}

/// The outcome of running an event's hooks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HookResult {
    pub allowed: bool,
    pub message: Option<String>,
}

impl HookResult {
    pub fn allow() -> Self {
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

/// Fire `event` to BOTH sinks — the on-disk scripts (`run`, Track C) and,
/// when the session has one (`extension_events.is_some()`), the extension
/// host (Track D, `plugins::extension::ExtensionEvents::dispatch`) — and
/// combine their results (see [`combine_hook_results`]). The two run
/// CONCURRENTLY (`tokio::join!`), so paying for a slow script never doubles
/// the cost of a slow extension dispatch, or vice versa.
///
/// `extension_events: None` (no extensions registered — the common case,
/// and every bare test context) skips the extension side entirely and is
/// exactly equivalent to calling `run` directly: zero behavioral change from
/// before Track D existed.
///
/// This is the single point every `harness::native` fire site should call
/// instead of `run` — see the module doc.
pub async fn fire_hook(
    work_dir: &Path,
    extension_events: Option<&Arc<dyn ExtensionEvents>>,
    event: HookEvent,
    payload: &Value,
) -> HookResult {
    let combined = match extension_events {
        Some(ext) => {
            let (script, extension) =
                tokio::join!(run(work_dir, event, payload), ext.dispatch(event, payload));
            combine_hook_results(script, extension)
        }
        None => run(work_dir, event, payload).await,
    };
    if event.is_gating() {
        combined
    } else {
        // Defensive: an observational event must never deny, even if a
        // (contract-violating) `ExtensionEvents` implementation reports one
        // — mirrors `run`'s own hard guarantee that a non-gating event can
        // never flip `allowed`, regardless of what either sink returned.
        HookResult::allow()
    }
}

/// Combine a script-hook result and an extension-dispatch result for the
/// SAME event: either one denying denies the whole call. For an
/// observational event both inputs are already unconditionally `allow` (see
/// `run`'s and `ExtensionEvents::dispatch`'s own contracts for a non-gating
/// event), so this naturally reduces to `allow` there too — no event-kind
/// branch needed here.
fn combine_hook_results(script: HookResult, extension: HookResult) -> HookResult {
    if !script.allowed {
        return script;
    }
    if !extension.allowed {
        return extension;
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

    #[test]
    fn hook_event_from_str_round_trips_every_variant() {
        for event in HookEvent::ALL {
            assert_eq!(event.as_str().parse::<HookEvent>(), Ok(*event));
        }
    }

    #[test]
    fn hook_event_from_str_rejects_an_unknown_string() {
        assert!("tool.beforee".parse::<HookEvent>().is_err());
    }

    // ---------- combine_hook_results (pure) ----------

    #[test]
    fn combine_denies_when_the_script_denies_even_if_the_extension_allows() {
        let script = HookResult {
            allowed: false,
            message: Some("blocked by hook".to_string()),
        };
        let combined = combine_hook_results(script.clone(), HookResult::allow());
        assert_eq!(combined, script);
    }

    #[test]
    fn combine_denies_when_the_extension_denies_even_if_the_script_allows() {
        let extension = HookResult {
            allowed: false,
            message: Some("linter: blocked".to_string()),
        };
        let combined = combine_hook_results(HookResult::allow(), extension.clone());
        assert_eq!(combined, extension);
    }

    #[test]
    fn combine_allows_only_when_both_allow() {
        assert_eq!(
            combine_hook_results(HookResult::allow(), HookResult::allow()),
            HookResult::allow()
        );
    }

    // ---------- fire_hook (script + extension combine) ----------

    /// A scripted [`crate::plugins::extension::ExtensionEvents`] fake — no
    /// real subprocess, just a fixed `HookResult` per call, so `fire_hook`'s
    /// combine logic is exercised without an sh-based fake extension.
    struct FakeExtensionEvents {
        result: HookResult,
    }

    #[async_trait::async_trait]
    impl crate::plugins::extension::ExtensionEvents for FakeExtensionEvents {
        async fn dispatch(&self, _event: HookEvent, _payload: &Value) -> HookResult {
            self.result.clone()
        }
    }

    fn fake_extension_events(
        result: HookResult,
    ) -> Arc<dyn crate::plugins::extension::ExtensionEvents> {
        Arc::new(FakeExtensionEvents { result })
    }

    #[tokio::test]
    async fn fire_hook_with_no_extension_events_behaves_exactly_like_run() {
        let dir = tempfile::tempdir().unwrap();
        let r = fire_hook(dir.path(), None, HookEvent::ToolBefore, &json!({})).await;
        assert_eq!(r, HookResult::allow());
    }

    #[tokio::test]
    async fn fire_hook_denies_when_the_extension_denies_and_no_script_exists() {
        let dir = tempfile::tempdir().unwrap();
        let ext = fake_extension_events(HookResult {
            allowed: false,
            message: Some("linter: blocked".to_string()),
        });
        let r = fire_hook(
            dir.path(),
            Some(&ext),
            HookEvent::ToolBefore,
            &json!({ "tool": "bash" }),
        )
        .await;
        assert!(!r.allowed);
        assert_eq!(r.message.as_deref(), Some("linter: blocked"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fire_hook_denies_when_a_passing_extension_meets_a_denying_script() {
        let dir = tempfile::tempdir().unwrap();
        write_hook(
            dir.path(),
            "tool.before",
            "deny.sh",
            "#!/bin/sh\necho 'bash is not allowed here'\nexit 1\n",
        );
        let ext = fake_extension_events(HookResult::allow());
        let r = fire_hook(
            dir.path(),
            Some(&ext),
            HookEvent::ToolBefore,
            &json!({ "tool": "bash" }),
        )
        .await;
        assert!(
            !r.allowed,
            "a script-deny must still deny even though the extension side allowed"
        );
        assert_eq!(r.message.as_deref(), Some("bash is not allowed here"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn fire_hook_allows_when_both_script_and_extension_allow() {
        let dir = tempfile::tempdir().unwrap();
        write_hook(
            dir.path(),
            "tool.before",
            "ok.sh",
            "#!/bin/sh\ncat >/dev/null\nexit 0\n",
        );
        let ext = fake_extension_events(HookResult::allow());
        let r = fire_hook(
            dir.path(),
            Some(&ext),
            HookEvent::ToolBefore,
            &json!({ "tool": "read" }),
        )
        .await;
        assert!(r.allowed);
    }

    #[tokio::test]
    async fn fire_hook_observational_always_allows_regardless_of_extension_result() {
        let dir = tempfile::tempdir().unwrap();
        // Even a (contract-violating) extension that reports `allowed: false`
        // for an observational event must never flip the combined result —
        // `ExtensionEvents::dispatch`'s own contract already guarantees this
        // for `ExtensionHost`, but `fire_hook`'s combine must not assume it.
        let ext = fake_extension_events(HookResult {
            allowed: false,
            message: Some("should be ignored".to_string()),
        });
        let r = fire_hook(
            dir.path(),
            Some(&ext),
            HookEvent::ToolAfter,
            &json!({ "tool": "bash" }),
        )
        .await;
        assert!(
            r.allowed,
            "observational fire_hook must never deny: {:?}",
            r.message
        );
    }
}
