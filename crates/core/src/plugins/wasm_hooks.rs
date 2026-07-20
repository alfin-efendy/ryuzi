//! Bridge a WASM component's `ryuzi:hooks/hooks` export into the native
//! runtime's hook dispatch, as an [`ExtensionEvents`] implementation threaded
//! next to the subprocess extension host at each `fire_hook` site.
//!
//! # Gating vs. observational
//! Only `tool.before` ([`HookEvent::is_gating`]) can deny an action. On a
//! gating event every enabled component is contacted CONCURRENTLY, each on its
//! own fresh, isolated instance bounded by the component's own fuel/epoch
//! budget; a `hook-error::rejected` from ANY component denies the call. Every
//! other outcome — an allowing `hook-result`, a `hook-error::unsupported`
//! (the component doesn't handle this event), a `hook-error::failed`, or a
//! host-side **trap / timeout / instantiation failure** — is treated as "did
//! not deny" and, for the failure cases, logged with `tracing::warn!`. This is
//! the documented **fail-OPEN** rule: a broken, slow, or looping component
//! must NEVER deadlock or brick the agent, mirroring the subprocess extension
//! host's own gating contract (`plugins::extension::events`).
//!
//! Observational events (`session.start`, `tool.after`, `session.end`) never
//! gate, so this dispatcher returns [`HookResult::allow`] for them immediately
//! without instantiating any component — component delivery of observational
//! events is intentionally out of scope for this slice (it would pay a fresh
//! instantiation on every tool call for a result that can never change
//! control flow).

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::Value;

use crate::harness::native::hooks::{HookEvent, HookResult};
use crate::plugins::capabilities::wit_bindings::exports::ryuzi::hooks::hooks as wit;
use crate::plugins::extension::ExtensionEvents;
use crate::plugins::wasm_connector::WasmActivation;

/// Dispatches native [`HookEvent`]s to every enabled component bundle's
/// `ryuzi:hooks/hooks` export. Threaded through `SessionCtx.wasm_hooks`
/// alongside the subprocess extension host's `extension_events`; both are
/// combined by `harness::native::hooks::fire_hook`.
pub struct WasmHookDispatcher {
    activations: Vec<Arc<WasmActivation>>,
}

impl WasmHookDispatcher {
    pub fn new(activations: Vec<Arc<WasmActivation>>) -> Self {
        WasmHookDispatcher { activations }
    }

    /// The activations a gating event is actually dispatched to: only those
    /// that export `ryuzi:hooks/hooks` (IMP-2). A connector-only component is
    /// excluded HERE, before any instantiation, so it is never instantiated
    /// and never logs a fail-open warning on a `tool.before`.
    fn hook_targets(&self) -> impl Iterator<Item = &Arc<WasmActivation>> {
        self.activations
            .iter()
            .filter(|activation| activation.exports_hooks())
    }

    /// Test seam: the component ids that a gating event would be dispatched to.
    #[cfg(test)]
    pub(crate) fn hook_target_ids(&self) -> Vec<String> {
        self.hook_targets()
            .map(|activation| activation.component_id().to_string())
            .collect()
    }
}

/// The outcome of dispatching one gating event to one component.
enum HookOutcome {
    /// The component allowed (or does not handle) the event.
    Allowed,
    /// The component explicitly rejected the action (`hook-error::rejected`).
    Denied,
    /// The component trapped, timed out, failed to instantiate, or returned
    /// `hook-error::failed` — fail OPEN, but log why.
    FailedOpen(String),
}

#[async_trait]
impl ExtensionEvents for WasmHookDispatcher {
    async fn dispatch(&self, event: HookEvent, payload: &Value) -> HookResult {
        // Observational events never gate — return immediately without paying
        // a component instantiation. (See the module doc.)
        if !event.is_gating() {
            return HookResult::allow();
        }
        // IMP-2: only dispatch to components that actually export hooks, so a
        // connector-only component is never instantiated (and never warns) on a
        // `tool.before`.
        let targets: Vec<Arc<WasmActivation>> = self.hook_targets().cloned().collect();
        if targets.is_empty() {
            return HookResult::allow();
        }
        let payload_json = serde_json::to_string(payload).unwrap_or_default();
        let calls = targets.into_iter().map(|activation| {
            let payload_json = payload_json.clone();
            async move {
                let outcome = dispatch_gating_one(&activation, event, payload_json).await;
                (activation.component_id().to_string(), outcome)
            }
        });
        for (component_id, outcome) in futures::future::join_all(calls).await {
            match outcome {
                HookOutcome::Denied => {
                    return HookResult {
                        allowed: false,
                        message: Some(format!("{component_id}: rejected by plugin hook")),
                    };
                }
                HookOutcome::FailedOpen(reason) => {
                    tracing::warn!(
                        component = %component_id,
                        event = event.as_str(),
                        "wasm plugin hook trapped/timed out/failed on a gating event — \
                         failing open (allow) so a broken component can never brick the agent: {reason}"
                    );
                }
                HookOutcome::Allowed => {}
            }
        }
        HookResult::allow()
    }
}

/// Dispatch one gating event to one component on a fresh, isolated instance.
/// The JSON payload is passed as a single WIT `hook-value::text`, so a
/// component can inspect it without the ABI needing a nested value type.
async fn dispatch_gating_one(
    activation: &WasmActivation,
    event: HookEvent,
    payload_json: String,
) -> HookOutcome {
    let wit_event = wit::HookEvent {
        name: event.as_str().to_string(),
        values: vec![wit::HookValue::Text(payload_json)],
    };
    let mut instance = match activation.instantiate().await {
        Ok(instance) => instance,
        Err(error) => return HookOutcome::FailedOpen(error.to_string()),
    };
    let result = instance
        .call(move |inst, store| {
            let pre = inst.instance_pre(&*store);
            let guest = wit::GuestIndices::new(&pre)?.load(&mut *store, &inst)?;
            guest.call_handle(&mut *store, &wit_event)
        })
        .await;
    match result {
        Ok(Ok(_hook_result)) => HookOutcome::Allowed,
        Ok(Err(wit::HookError::Rejected)) => HookOutcome::Denied,
        // A component that does not handle this event is not a denial.
        Ok(Err(wit::HookError::Unsupported)) => HookOutcome::Allowed,
        Ok(Err(wit::HookError::Failed(message))) => {
            HookOutcome::FailedOpen(format!("hook returned failed: {message}"))
        }
        Err(runtime_error) => HookOutcome::FailedOpen(runtime_error.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::Principal;
    use crate::plugins::bundle::InstalledBundle;
    use crate::plugins::capabilities::PluginCapabilityContext;
    use crate::plugins::runtime::{ComponentRuntime, HostPolicy};
    use crate::settings::SettingsStore;
    use crate::store::ComponentPluginReleaseRecord;
    use crate::telemetry::NoopTelemetry;
    use ryuzi_plugin_sdk::{
        PluginBundleManifest, PluginLifecycle, PluginPermissions, PluginRelease,
    };
    use serde_json::json;
    use std::path::PathBuf;
    use std::time::Duration;

    use crate::plugins::build_fixture_components_once as build_fixtures;

    fn fixture_artifact(dir: &str, file: &str) -> PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures")
            .join(dir)
            .join("target/wasm32-wasip2/release")
            .join(file)
    }

    fn hooks_artifact() -> PathBuf {
        fixture_artifact("component-hooks", "ryuzi_component_hooks_fixture.wasm")
    }

    fn hooks_loop_artifact() -> PathBuf {
        fixture_artifact(
            "component-hooks-loop",
            "ryuzi_component_hooks_loop_fixture.wasm",
        )
    }

    fn connector_artifact() -> PathBuf {
        fixture_artifact(
            "component-connector",
            "ryuzi_component_connector_fixture.wasm",
        )
    }

    fn test_bundle(component_path: PathBuf, plugin_id: &str) -> InstalledBundle {
        InstalledBundle {
            manifest: PluginBundleManifest {
                id: plugin_id.to_string(),
                name: plugin_id.to_string(),
                version: "0.1.0".to_string(),
                wit_api: "^0.1.0".to_string(),
                lifecycle: PluginLifecycle::Singleton,
                component: "plugin.wasm".to_string(),
                publisher: String::new(),
                description: String::new(),
                permissions: PluginPermissions { network: vec![] },
                oauth: vec![],
            },
            release: PluginRelease {
                id: plugin_id.to_string(),
                version: "0.1.0".to_string(),
                wit_api: "0.1.0".to_string(),
                component_url: "https://example.invalid/x.wasm".to_string(),
                component_sha256: "0".repeat(64),
                size_bytes: None,
                published_at: None,
            },
            release_record: ComponentPluginReleaseRecord {
                plugin_id: plugin_id.to_string(),
                version: "0.1.0".to_string(),
                source_url: "https://example.invalid/x.wasm".to_string(),
                sha256: "0".repeat(64),
                signing_key_id: "test".to_string(),
                installed_at: 0,
                active: true,
                revoked: false,
                revocation_reason: None,
            },
            root: component_path.parent().unwrap().to_path_buf(),
            component_path,
        }
    }

    fn test_ctx(store: &Arc<crate::store::Store>, plugin_id: &str) -> Arc<PluginCapabilityContext> {
        Arc::new(PluginCapabilityContext {
            plugin_id: plugin_id.to_string(),
            version: "0.1.0".to_string(),
            settings: SettingsStore::new(store.clone()),
            store: store.clone(),
            telemetry: Arc::new(NoopTelemetry),
            network_allowlist: vec![],
            oauth_profile_ids: vec![],
        })
    }

    fn principal(plugin_id: &str) -> Principal {
        Principal {
            plugin_id: plugin_id.to_string(),
            plugin_name: plugin_id.to_string(),
        }
    }

    /// Compile + wrap one activation using a CALLER-SUPPLIED runtime + store,
    /// so a test can put multiple components on the SAME `ComponentRuntime` —
    /// exactly as production does (`lifecycle::build_wasm_session_providers`
    /// compiles every enabled bundle with one runtime). Essential for the
    /// epoch-isolation regression: only a shared runtime exposes whether the
    /// engine (and its epoch counter) is shared across components.
    fn shared_activation(
        runtime: &ComponentRuntime,
        store: &Arc<crate::store::Store>,
        component_path: PathBuf,
        plugin_id: &str,
        policy: HostPolicy,
    ) -> Arc<WasmActivation> {
        let bundle = test_bundle(component_path, plugin_id);
        let compiled = Arc::new(runtime.compile(&bundle, policy).unwrap());
        Arc::new(WasmActivation::new(
            compiled,
            test_ctx(store, plugin_id),
            plugin_id.to_string(),
            principal(plugin_id),
        ))
    }

    /// Build one `WasmActivation` (its own runtime + store) from a prebuilt
    /// fixture artifact under an arbitrary policy. Keeps the tempfile alive.
    async fn build_activation(
        component_path: PathBuf,
        plugin_id: &str,
        policy: HostPolicy,
    ) -> (Arc<WasmActivation>, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let runtime = ComponentRuntime::new().unwrap();
        let activation = shared_activation(&runtime, &store, component_path, plugin_id, policy);
        (activation, tmp)
    }

    async fn hooks_dispatcher(timeout: Duration) -> (WasmHookDispatcher, tempfile::NamedTempFile) {
        let mut policy = HostPolicy::deny_all();
        policy.limits.timeout = timeout;
        let (activation, tmp) = build_activation(hooks_artifact(), "acme-hooks", policy).await;
        (WasmHookDispatcher::new(vec![activation]), tmp)
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gating_allows_when_the_component_allows() {
        build_fixtures();
        let (dispatcher, _tmp) = hooks_dispatcher(Duration::from_secs(10)).await;
        let result = dispatcher
            .dispatch(HookEvent::ToolBefore, &json!({ "tool": "read" }))
            .await;
        assert!(result.allowed, "an allowing component must not deny");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gating_denies_when_the_component_rejects() {
        build_fixtures();
        let (dispatcher, _tmp) = hooks_dispatcher(Duration::from_secs(10)).await;
        // The fixture rejects when the payload text contains "deny".
        let result = dispatcher
            .dispatch(HookEvent::ToolBefore, &json!({ "tool": "deny-me" }))
            .await;
        assert!(!result.allowed, "a rejecting component must deny the call");
        assert!(result
            .message
            .as_deref()
            .unwrap()
            .contains("rejected by plugin hook"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn gating_fails_open_when_the_component_times_out() {
        build_fixtures();
        let (dispatcher, _tmp) = hooks_dispatcher(Duration::from_millis(200)).await;
        // The fixture loops forever when the payload text contains "boom".
        let started = std::time::Instant::now();
        let result = dispatcher
            .dispatch(HookEvent::ToolBefore, &json!({ "tool": "boom" }))
            .await;
        let elapsed = started.elapsed();
        assert!(
            result.allowed,
            "a timed-out gating component must fail OPEN (allow), never brick the agent"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "fail-open must be prompt, not wait indefinitely: {elapsed:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn observational_events_allow_without_calling_the_component() {
        build_fixtures();
        // A tiny timeout would make a real component call error, but an
        // observational event must never call the component at all.
        let (dispatcher, _tmp) = hooks_dispatcher(Duration::from_millis(1)).await;
        for event in [
            HookEvent::SessionStart,
            HookEvent::ToolAfter,
            HookEvent::SessionEnd,
        ] {
            let result = dispatcher
                .dispatch(event, &json!({ "tool": "boom-deny" }))
                .await;
            assert!(
                result.allowed,
                "observational event {} must allow without calling the component",
                event.as_str()
            );
        }
    }

    fn policy_with_fuel(timeout: Duration, fuel: u64) -> HostPolicy {
        let mut policy = HostPolicy::deny_all();
        policy.limits.timeout = timeout;
        policy.limits.fuel = fuel;
        policy
    }

    // ---------- IMP-2: skip components lacking the hooks export ----------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn connector_only_component_is_not_a_hook_target() {
        build_fixtures();
        // A 1ms timeout would make any real hook call fail; the connector-only
        // component must be excluded from the target set BEFORE any
        // instantiation, so it is never instantiated and never warns.
        let (connector, _c) = build_activation(
            connector_artifact(),
            "acme-tools",
            policy_with_fuel(Duration::from_millis(1), 10_000_000),
        )
        .await;
        let dispatcher = WasmHookDispatcher::new(vec![connector]);
        assert!(
            dispatcher.hook_target_ids().is_empty(),
            "a connector-only component must not be a hook dispatch target"
        );
        let result = dispatcher
            .dispatch(HookEvent::ToolBefore, &json!({ "tool": "deny" }))
            .await;
        assert!(
            result.allowed,
            "with no hooks-exporting component, tool.before allows without instantiation"
        );
    }

    // ---------- IMP-1: per-component engine isolates epoch counters ----------

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn concurrent_timeout_does_not_flip_another_components_deny() {
        build_fixtures();
        // Compile BOTH components on ONE ComponentRuntime, exactly as
        // production does. That is the only configuration under which the old
        // bug could occur: a single shared `Engine` (hence a single shared
        // epoch counter) across all enabled components. The IMP-1 fix gives
        // each `CompiledComponent` its own engine even off a shared runtime.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let runtime = ComponentRuntime::new().unwrap();
        // A: always loops, short timeout — it traps and calls
        // `increment_epoch()` on ITS engine at ~30ms.
        let loop_component = shared_activation(
            &runtime,
            &store,
            hooks_loop_artifact(),
            "loop-plugin",
            policy_with_fuel(Duration::from_millis(30), u64::MAX),
        );
        // B: spins (well within its own 30s budget + unbounded fuel) then
        // returns `rejected`. With per-component engines (IMP-1), A's timeout
        // must NOT trip B's epoch deadline, so B's deny survives. Under the old
        // shared-engine bug, A's `increment_epoch` would trap B mid-spin,
        // making B fail open (allow) — silently dropping the deny.
        let deny_component = shared_activation(
            &runtime,
            &store,
            hooks_artifact(),
            "deny-plugin",
            policy_with_fuel(Duration::from_secs(30), u64::MAX),
        );
        let dispatcher = WasmHookDispatcher::new(vec![loop_component, deny_component]);

        let result = dispatcher
            .dispatch(HookEvent::ToolBefore, &json!({ "tool": "spinreject" }))
            .await;
        assert!(
            !result.allowed,
            "the deny component's rejection must survive the other component's \
             concurrent timeout — epoch counters are isolated per component"
        );
        assert!(
            result
                .message
                .as_deref()
                .unwrap_or_default()
                .contains("deny-plugin"),
            "the surviving denial must be the deny component's: {:?}",
            result.message
        );
    }
}
