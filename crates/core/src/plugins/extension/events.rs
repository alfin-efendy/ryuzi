//! DT5: dispatch a Track C [`HookEvent`] to every subscribed, running
//! extension subprocess — the second sink for the event dispatch point
//! Track C built (`harness::native::hooks::run` is the first sink, firing
//! the same event to on-disk scripts; `harness::native::hooks::fire_hook`
//! combines both — see that function's doc).
//!
//! # Gating vs. observational
//! - **Gating (`HookEvent::is_gating()`, i.e. `tool.before`)**: every
//!   subscribed extension is contacted CONCURRENTLY (not one at a time —
//!   `futures::future::join_all`) and awaited, each bounded by ITS OWN
//!   manifest `timeout_ms` ([`proc::dispatch_event`] enforces this per
//!   extension, so joining them concurrently bounds total wait to the
//!   slowest single extension's timeout, never their sum). ANY extension
//!   denying denies the call. A timeout or a transport failure (crash,
//!   closed pipe) is **fail-OPEN**: treated as "did not deny," plus a
//!   `tracing::warn!` — a broken/slow extension must NEVER deadlock or brick
//!   the agent. This mirrors `harness::native::hooks::run`'s own script
//!   contract (missing hook dir / spawn failure = allow), just with a
//!   network round trip instead of a process exit code.
//! - **Observational** (`session.start`/`tool.after`/`session.end`): never
//!   awaited on the caller's path at all. Each subscribed extension's send
//!   is handed to a detached `tokio::spawn` task, gated by
//!   [`proc::ExtensionHost::try_acquire_observational_permit`] so a burst of
//!   slow/misbehaving extensions can only ever have a bounded number of
//!   sends in flight — a send that can't get a permit is dropped (logged),
//!   never queued. [`ExtensionEvents::dispatch`] returns
//!   `HookResult::allow()` immediately in this branch, before any of those
//!   tasks could possibly have resolved.
//!
//! An extension NOT subscribed to the firing event (its confirmed
//! `events` from `extension/initialize` doesn't include it) is never
//! contacted at all — see [`proc::dispatch_event`]'s `Skipped` outcome.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::harness::native::hooks::{HookEvent, HookResult};

use super::proc::{DispatchHandle, EventDispatchOutcome, ExtensionHost};

/// Cap on a sanitized deny reason's length (characters, after secret-shaped
/// redaction below). A gating deny reason IS meant to be shown to the
/// user/agent — unlike an init-handshake failure
/// (`proc::sanitize_init_error`), which collapses to a canned per-stage
/// message — so this only truncates and screens, it never discards the
/// whole message.
const MAX_DENY_REASON_CHARS: usize = 300;

/// Case-insensitive substrings that mark a deny reason as "secret-shaped" —
/// deliberately broad and over-inclusive: a false positive just replaces a
/// harmless reason with a generic marker; a false negative could leak a
/// credential straight into a transcript/UI. An extension is less trusted
/// than a script the user wrote by hand (it is arbitrary vendor code), so
/// its reason gets this extra screening a script's stdout does not.
const SECRET_SHAPED_MARKERS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "api-key",
    "authorization",
    "bearer",
    "credential",
];

/// Dispatch a lifecycle [`HookEvent`] to registered extension subprocesses.
/// Implemented for [`ExtensionHost`]; `harness::native::hooks::fire_hook`
/// (Track C's combine point) calls through a `SessionCtx.extension_events:
/// Option<Arc<dyn ExtensionEvents>>` so the hot fire sites never depend on
/// `ExtensionHost`'s concrete type.
#[async_trait]
pub trait ExtensionEvents: Send + Sync {
    /// Dispatch `event` to every subscribed+running extension.
    ///
    /// Gating events await each subscribed extension's response up to its
    /// own `timeout_ms`; a `{"deny": true, "reason": "..."}` denies the
    /// action. A timeout or a crashed/closed transport is fail-OPEN (allow)
    /// plus a warning.
    ///
    /// Observational events are fire-and-forget and this call returns
    /// `HookResult::allow()` immediately, without waiting on any extension.
    async fn dispatch(&self, event: HookEvent, payload: &Value) -> HookResult;
}

#[async_trait]
impl ExtensionEvents for ExtensionHost {
    async fn dispatch(&self, event: HookEvent, payload: &Value) -> HookResult {
        let handles = self.dispatch_handles().await;
        if handles.is_empty() {
            return HookResult::allow();
        }
        if event.is_gating() {
            dispatch_gating(handles, event, payload).await
        } else {
            self.dispatch_observational(handles, event, payload);
            HookResult::allow()
        }
    }
}

/// The gating half of [`ExtensionEvents::dispatch`] — see this module's doc.
async fn dispatch_gating(
    handles: Vec<DispatchHandle>,
    event: HookEvent,
    payload: &Value,
) -> HookResult {
    let calls = handles.into_iter().map(|handle| async move {
        let outcome = handle.dispatch(event, payload).await;
        (handle.name().to_string(), outcome)
    });
    for (name, outcome) in futures::future::join_all(calls).await {
        match outcome {
            EventDispatchOutcome::Denied(reason) => {
                return HookResult {
                    allowed: false,
                    message: Some(sanitize_deny_reason(&name, reason)),
                };
            }
            EventDispatchOutcome::Unreachable => {
                tracing::warn!(
                    extension = %name,
                    event = event.as_str(),
                    "extension timed out or its transport failed responding to a gating event \
                     — failing open (allow) so a broken extension can never brick the agent"
                );
            }
            EventDispatchOutcome::Allowed | EventDispatchOutcome::Skipped => {}
        }
    }
    HookResult::allow()
}

impl ExtensionHost {
    /// The observational half of [`ExtensionEvents::dispatch`] — see this
    /// module's doc. Never awaited by its caller: each send is a detached
    /// `tokio::spawn` task, bounded by
    /// [`ExtensionHost::try_acquire_observational_permit`].
    fn dispatch_observational(
        &self,
        handles: Vec<DispatchHandle>,
        event: HookEvent,
        payload: &Value,
    ) {
        let payload = Arc::new(payload.clone());
        for handle in handles {
            let Some(permit) = self.try_acquire_observational_permit() else {
                tracing::warn!(
                    extension = %handle.name(),
                    event = event.as_str(),
                    "dropped an observational event dispatch: too many sends already in flight"
                );
                continue;
            };
            let payload = payload.clone();
            tokio::spawn(async move {
                let _permit = permit; // held for this send's lifetime
                let outcome = handle.dispatch(event, &payload).await;
                if matches!(outcome, EventDispatchOutcome::Unreachable) {
                    tracing::debug!(
                        extension = %handle.name(),
                        event = event.as_str(),
                        "observational event dispatch to extension timed out or failed \
                         (ignored — fire-and-forget)"
                    );
                }
            });
        }
    }
}

/// Turn an extension's raw deny reason into something safe to surface in a
/// transcript/UI: `None`/empty becomes a generic marker; a reason that looks
/// like it contains a credential (see [`SECRET_SHAPED_MARKERS`]) is replaced
/// wholesale rather than surgically redacted (an extension controls its own
/// formatting, so a partial redaction is easy to get wrong); everything else
/// is capped at [`MAX_DENY_REASON_CHARS`]. Always prefixed with the
/// extension's name, mirroring a script hook's own denial message
/// (`harness::native::hooks::run`'s `"blocked by hook {path}"` fallback).
fn sanitize_deny_reason(name: &str, reason: Option<String>) -> String {
    let Some(raw) = reason
        .map(|r| r.trim().to_string())
        .filter(|r| !r.is_empty())
    else {
        return format!("{name}: denied (no reason given)");
    };
    let lower = raw.to_lowercase();
    let screened = if SECRET_SHAPED_MARKERS.iter().any(|m| lower.contains(m)) {
        "[reason withheld: it looked like it might contain a credential]".to_string()
    } else {
        raw
    };
    let capped = if screened.chars().count() > MAX_DENY_REASON_CHARS {
        let mut s: String = screened.chars().take(MAX_DENY_REASON_CHARS).collect();
        s.push('…');
        s
    } else {
        screened
    };
    format!("{name}: {capped}")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(unix)]
    use crate::plugins::extension::{ExtensionCtx, ExtensionFactory, ExtensionSpec};
    #[cfg(unix)]
    use crate::plugins::host::{CorePlugin, PluginHost, PluginSource};
    #[cfg(unix)]
    use crate::settings::SettingsStore;
    #[cfg(unix)]
    use crate::store::Store;
    #[cfg(unix)]
    use ryuzi_plugin_sdk::PluginManifest;
    #[cfg(unix)]
    use serde_json::json;
    #[cfg(unix)]
    use std::time::Duration;

    // ---------- sanitize_deny_reason (pure, no I/O) ----------

    #[test]
    fn sanitize_deny_reason_passes_through_a_plain_reason_with_the_extension_name() {
        assert_eq!(
            sanitize_deny_reason("linter", Some("bash is not allowed here".to_string())),
            "linter: bash is not allowed here"
        );
    }

    #[test]
    fn sanitize_deny_reason_falls_back_when_no_reason_given() {
        assert_eq!(
            sanitize_deny_reason("linter", None),
            "linter: denied (no reason given)"
        );
        assert_eq!(
            sanitize_deny_reason("linter", Some("   ".to_string())),
            "linter: denied (no reason given)"
        );
    }

    #[test]
    fn sanitize_deny_reason_withholds_a_secret_shaped_reason() {
        let reason = sanitize_deny_reason(
            "linter",
            Some("denied: token=leaked-secret-token in the request".to_string()),
        );
        assert!(!reason.contains("leaked-secret-token"));
        assert!(reason.contains("withheld"));
    }

    #[test]
    fn sanitize_deny_reason_caps_length() {
        let long = "x".repeat(1000);
        let reason = sanitize_deny_reason("linter", Some(long));
        assert!(reason.chars().count() <= MAX_DENY_REASON_CHARS + "linter: ".len() + 1);
        assert!(reason.ends_with('…'));
    }

    // ---------- ExtensionEvents integration (real sh-based fake extensions) ----------
    // Mirrors `proc.rs`'s own DT3/DT4 test fixtures: a tiny `sh -c` one-liner
    // plays the fake extension over real stdio pipes, hermetic (no committed
    // script file) and `#[cfg(unix)]`-gated to match this crate's CI matrix.

    #[cfg(unix)]
    fn manifest(id: &str) -> PluginManifest {
        PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: id.to_string(),
            version: String::new(),
            publisher: String::new(),
            description: String::new(),
            homepage: None,
            icon: None,
            categories: vec![],
            slot: None,
            verified: false,
            experimental: false,
            auth: None,
            settings: vec![],
            mcp: vec![],
            extensions: vec![],
            skills: vec![],
            provider: None,
        }
    }

    #[cfg(unix)]
    struct FakeExtensionFactory {
        specs: Vec<ExtensionSpec>,
    }

    #[cfg(unix)]
    #[async_trait]
    impl ExtensionFactory for FakeExtensionFactory {
        async fn extensions(&self, _ctx: &ExtensionCtx) -> anyhow::Result<Vec<ExtensionSpec>> {
            Ok(self.specs.clone())
        }
    }

    #[cfg(unix)]
    fn extension_only(id: &str, specs: Vec<ExtensionSpec>) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: None,
            connector: None,
            extension: Some(Arc::new(FakeExtensionFactory { specs })),
            provider: None,
            source: PluginSource::Builtin,
        }
    }

    #[cfg(unix)]
    async fn open_ctx() -> (ExtensionCtx, Arc<Store>, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let settings = SettingsStore::new(store.clone());
        (ExtensionCtx { settings }, store, tmp)
    }

    #[cfg(unix)]
    fn base_spec(
        name: &str,
        body: &str,
        events: Vec<HookEvent>,
        timeout: Duration,
    ) -> ExtensionSpec {
        ExtensionSpec {
            name: name.to_string(),
            command: "sh".to_string(),
            args: vec!["-c".to_string(), body.to_string()],
            events,
            provides_tools: false,
            timeout,
            env: vec![],
        }
    }

    /// A `sh` script: read+ack the `extension/initialize` handshake with
    /// `confirmed_events_json` (a raw JSON array literal, e.g.
    /// `r#"["tool.before"]"#`), then read the NEXT request (the event
    /// dispatch) and run `second_response_body` — a shell snippet with `$id2`
    /// bound to that second request's JSON-RPC id.
    #[cfg(unix)]
    fn handshake_then(confirmed_events_json: &str, second_response_body: &str) -> String {
        format!(
            "IFS= read -r line; id=$(printf '%s' \"$line\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p'); \
             printf '{{\"jsonrpc\":\"2.0\",\"id\":%s,\"result\":{{\"ok\":true,\"events\":{events}}}}}\\n' \"$id\"; \
             IFS= read -r line2; id2=$(printf '%s' \"$line2\" | sed -n 's/.*\"id\":\\([0-9]*\\).*/\\1/p'); \
             {body}",
            events = confirmed_events_json,
            body = second_response_body,
        )
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn gating_dispatch_denies_when_a_subscribed_extension_denies() {
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        let body = handshake_then(
            r#"["tool.before"]"#,
            r#"printf '{"jsonrpc":"2.0","id":%s,"result":{"deny":true,"reason":"blocked by linter"}}\n' "$id2""#,
        );
        host.add(extension_only(
            "denier",
            vec![base_spec(
                "linter",
                &body,
                vec![HookEvent::ToolBefore],
                Duration::from_millis(500),
            )],
        ));
        store
            .set_setting_raw("plugin.denier.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;
        assert_eq!(
            ext_host.get("denier").await[0].status,
            crate::plugins::extension::ExtensionStatus::Running,
            "sanity: the fake extension must have handshaken successfully"
        );

        let result = ext_host
            .dispatch(HookEvent::ToolBefore, &json!({ "tool": "bash" }))
            .await;
        assert!(
            !result.allowed,
            "a subscribed extension's deny must deny the call"
        );
        assert!(result
            .message
            .as_deref()
            .unwrap()
            .contains("blocked by linter"));

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn observational_dispatch_returns_immediately_even_if_the_extension_is_slow() {
        let (ctx, store, _tmp) = open_ctx().await;
        let marker = tempfile::NamedTempFile::new().unwrap();
        std::fs::remove_file(marker.path()).ok();
        let mut host = PluginHost::new();
        // Receives the event (touches the marker to prove it), then hangs
        // well past this test's own patience — proving the caller never
        // waited on it.
        let body = handshake_then(
            r#"["tool.after"]"#,
            &format!("touch '{}'; sleep 5", marker.path().display()),
        );
        host.add(extension_only(
            "slowpoke",
            vec![base_spec(
                "slowpoke",
                &body,
                vec![HookEvent::ToolAfter],
                Duration::from_millis(300),
            )],
        ));
        store
            .set_setting_raw("plugin.slowpoke.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;

        let start = std::time::Instant::now();
        let result = ext_host
            .dispatch(HookEvent::ToolAfter, &json!({ "tool": "bash" }))
            .await;
        let elapsed = start.elapsed();
        assert!(result.allowed, "observational dispatch must always allow");
        assert!(
            elapsed < Duration::from_millis(200),
            "observational dispatch must return immediately, not wait on the extension: {elapsed:?}"
        );

        // The extension must still have actually been contacted — this just
        // was not awaited on the hot path.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !marker.path().exists() && std::time::Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        assert!(
            marker.path().exists(),
            "the subscribed extension must have received the observational event"
        );

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn gating_dispatch_fails_open_when_a_subscribed_extension_times_out() {
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        // Handshakes fine, then never answers the event dispatch at all.
        let body = handshake_then(r#"["tool.before"]"#, "sleep 5");
        host.add(extension_only(
            "hangs",
            vec![base_spec(
                "hangs",
                &body,
                vec![HookEvent::ToolBefore],
                Duration::from_millis(100),
            )],
        ));
        store
            .set_setting_raw("plugin.hangs.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;

        let start = std::time::Instant::now();
        let result = ext_host
            .dispatch(HookEvent::ToolBefore, &json!({ "tool": "bash" }))
            .await;
        let elapsed = start.elapsed();
        assert!(
            result.allowed,
            "a timed-out gating extension must fail OPEN (allow), never brick the agent"
        );
        assert!(
            elapsed < Duration::from_secs(2),
            "must not wait past the extension's own timeout budget: {elapsed:?}"
        );

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn gating_dispatch_fails_open_when_the_extension_crashes_mid_dispatch() {
        let (ctx, store, _tmp) = open_ctx().await;
        let mut host = PluginHost::new();
        // Handshakes fine, then reads the event request and exits without
        // ever responding — the transport closes mid-dispatch.
        let body = handshake_then(r#"["tool.before"]"#, "exit 0");
        host.add(extension_only(
            "crashes",
            vec![base_spec(
                "crashes",
                &body,
                vec![HookEvent::ToolBefore],
                Duration::from_millis(500),
            )],
        ));
        store
            .set_setting_raw("plugin.crashes.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;

        let result = ext_host
            .dispatch(HookEvent::ToolBefore, &json!({ "tool": "bash" }))
            .await;
        assert!(
            result.allowed,
            "a crashed/closed-transport gating extension must fail OPEN (allow)"
        );

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn dispatch_does_not_contact_an_extension_not_subscribed_to_the_event() {
        let (ctx, store, _tmp) = open_ctx().await;
        let marker = tempfile::NamedTempFile::new().unwrap();
        std::fs::remove_file(marker.path()).ok();
        let mut host = PluginHost::new();
        // Confirms only "tool.after" — if a `ToolBefore` dispatch ever
        // reached its second read, this would touch the marker.
        let body = handshake_then(
            r#"["tool.after"]"#,
            &format!("touch '{}'", marker.path().display()),
        );
        host.add(extension_only(
            "not-subscribed",
            vec![base_spec(
                "not-subscribed",
                &body,
                vec![HookEvent::ToolAfter],
                Duration::from_millis(300),
            )],
        ));
        store
            .set_setting_raw("plugin.not-subscribed.enabled", "true")
            .await
            .unwrap();

        let ext_host = ExtensionHost::new();
        ext_host.spawn_all(&host, &ctx).await;

        let result = ext_host
            .dispatch(HookEvent::ToolBefore, &json!({ "tool": "bash" }))
            .await;
        assert!(result.allowed);
        tokio::time::sleep(Duration::from_millis(150)).await;
        assert!(
            !marker.path().exists(),
            "an extension not subscribed to this event must never be contacted"
        );

        ext_host.shutdown_all(Duration::from_millis(200)).await;
    }
}
