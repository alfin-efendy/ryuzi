//! Bridge a WASM component's `ryuzi:provider/provider` export (an in-process
//! model provider) into the LLM router.
//!
//! # Shape
//! The WIT `provider` interface is deliberately tiny: `list-models` (for
//! discoverability) and `complete` (a single call returning ALL completion
//! chunks as a `list<completion-chunk>`). This module exposes it behind a
//! generic [`WasmProviderRuntime`] trait so the router can dispatch to any
//! installed provider bundle without knowing a plugin id, and a concrete
//! [`WasmProviderTransport`] over the Task-9 callable-component-handle runtime
//! ([`CompiledComponent`]) — each provider owns its own epoch-isolated engine,
//! so a trapping/looping `complete` is caught by the host fuel/epoch budget and
//! surfaces as an `Err`, never a daemon crash.
//!
//! # The router seam (generic, no plugin id)
//! `list-models` results are registered as leaked `&'static ProviderDescriptor`s
//! via `crate::llm_router::registry::register_custom_descriptor` (the same seam
//! user custom providers use) so `route_model` resolves a provider bundle like a
//! built-in. The concrete transports are held in a process-wide registry keyed
//! by provider id ([`register_wasm_provider`]/[`wasm_provider`]); the router's
//! `anthropic_messages_stream` diverts a routed connection to
//! `wasm_provider_stream` iff [`wasm_provider`]`(&target.conn.provider)` finds a
//! transport — a DATA-driven predicate, so the choke point stays generic. Both
//! the descriptor registration and the transport bootstrap are wired by Task 11;
//! this module builds the reusable seam and its behaviour.

use std::collections::HashMap;
use std::sync::{Arc, OnceLock, PoisonError, RwLock};

use async_trait::async_trait;

use crate::plugins::capabilities::wit_bindings::exports::ryuzi::provider::provider as wit;
use crate::plugins::capabilities::PluginCapabilityContext;
use crate::plugins::runtime::{CompiledComponent, ComponentInstance, PluginRuntimeError};

/// One model a WASM provider advertises (the host-side mirror of the WIT
/// `model-info`). Registered as a `ProviderDescriptor` model by Task 11.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WasmModelInfo {
    pub id: String,
    pub display_name: String,
    pub context_window: u32,
}

/// A completion request handed to a WASM provider (host-side mirror of the WIT
/// `completion-request`). The router flattens an Anthropic-Messages body into
/// the flat `prompt` this carries.
#[derive(Debug, Clone, PartialEq)]
pub struct WasmCompletionRequest {
    pub model: String,
    pub prompt: String,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
}

/// Token usage a completion chunk may report (host-side mirror of the WIT
/// `token-usage`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WasmTokenUsage {
    pub input: u32,
    pub output: u32,
}

/// One completion chunk from a WASM provider (host-side mirror of the WIT
/// `completion-chunk`). `complete` returns these as an ORDERED list; the router
/// presents them as an ordered stream, preserving this order.
#[derive(Debug, Clone, PartialEq)]
pub struct WasmCompletionChunk {
    pub text: String,
    pub finished: bool,
    pub usage: Option<WasmTokenUsage>,
}

/// The generic provider seam the LLM router dispatches to. Object-safe so a
/// transport can be held as `Arc<dyn WasmProviderRuntime>` in the process-wide
/// registry and looked up by provider id, with no plugin id in the router.
#[async_trait]
pub trait WasmProviderRuntime: Send + Sync {
    /// The provider id this transport is registered under — matches the
    /// `ProviderDescriptor.id`/`ConnectionRow.provider` a route resolves to.
    fn provider_id(&self) -> &str;

    /// Enumerate the provider's models. A guest `provider-error`, or any
    /// host-side trap/timeout/instantiation failure, becomes an `Err(String)` —
    /// never a panic.
    async fn list_models(&self) -> Result<Vec<WasmModelInfo>, String>;

    /// Run a completion, returning every chunk in order. A guest
    /// `provider-error`, or any host-side trap/timeout, becomes an `Err(String)`
    /// the router converts into a route-scoped failure — never a panic or a hung
    /// daemon.
    async fn complete(
        &self,
        request: WasmCompletionRequest,
    ) -> Result<Vec<WasmCompletionChunk>, String>;
}

/// A generic provider backed by one enabled component bundle, compiled once and
/// re-instantiated per call (so concurrent completions never share mutable Wasm
/// state), mirroring [`crate::plugins::wasm_connector::WasmActivation`].
pub struct WasmProviderTransport {
    compiled: Arc<CompiledComponent>,
    ctx: Arc<PluginCapabilityContext>,
    provider_id: String,
}

impl WasmProviderTransport {
    /// Build a transport for one enabled provider bundle. `compiled` is the
    /// validated component; `ctx` carries the shared settings/store/telemetry
    /// backends; `provider_id` is the id the router resolves connections to.
    pub fn new(
        compiled: Arc<CompiledComponent>,
        ctx: Arc<PluginCapabilityContext>,
        provider_id: String,
    ) -> Self {
        WasmProviderTransport {
            compiled,
            ctx,
            provider_id,
        }
    }

    /// Whether this component actually exports `ryuzi:provider/provider` — the
    /// caller (Task 11 bootstrap) skips a non-provider bundle before ever
    /// instantiating it (mirrors the connector/hooks IMP-2 gating).
    pub fn exports_provider(&self) -> bool {
        self.compiled.exports_provider()
    }

    async fn instantiate(&self) -> Result<ComponentInstance, PluginRuntimeError> {
        self.compiled.instantiate(self.ctx.clone()).await
    }
}

#[async_trait]
impl WasmProviderRuntime for WasmProviderTransport {
    fn provider_id(&self) -> &str {
        &self.provider_id
    }

    async fn list_models(&self) -> Result<Vec<WasmModelInfo>, String> {
        if !self.exports_provider() {
            return Err("component does not export ryuzi:provider/provider".to_string());
        }
        let mut instance = self.instantiate().await.map_err(|e| e.to_string())?;
        let result = instance
            .call(|inst, store| {
                let pre = inst.instance_pre(&*store);
                let guest = wit::GuestIndices::new(&pre)?.load(&mut *store, &inst)?;
                guest.call_list_models(&mut *store)
            })
            .await
            .map_err(|e| e.to_string())?;
        match result {
            Ok(models) => Ok(models.into_iter().map(model_from_wit).collect()),
            Err(provider_error) => Err(describe_provider_error(&provider_error)),
        }
    }

    async fn complete(
        &self,
        request: WasmCompletionRequest,
    ) -> Result<Vec<WasmCompletionChunk>, String> {
        if !self.exports_provider() {
            return Err("component does not export ryuzi:provider/provider".to_string());
        }
        let wit_request = request_to_wit(request);
        let mut instance = self.instantiate().await.map_err(|e| e.to_string())?;
        let result = instance
            .call(move |inst, store| {
                let pre = inst.instance_pre(&*store);
                let guest = wit::GuestIndices::new(&pre)?.load(&mut *store, &inst)?;
                guest.call_complete(&mut *store, &wit_request)
            })
            .await
            .map_err(|e| e.to_string())?;
        match result {
            Ok(chunks) => Ok(chunks.into_iter().map(chunk_from_wit).collect()),
            Err(provider_error) => Err(describe_provider_error(&provider_error)),
        }
    }
}

fn model_from_wit(model: wit::ModelInfo) -> WasmModelInfo {
    WasmModelInfo {
        id: model.id,
        display_name: model.display_name,
        context_window: model.context_window,
    }
}

fn chunk_from_wit(chunk: wit::CompletionChunk) -> WasmCompletionChunk {
    WasmCompletionChunk {
        text: chunk.text,
        finished: chunk.finished,
        usage: chunk.usage.map(|usage| WasmTokenUsage {
            input: usage.input,
            output: usage.output,
        }),
    }
}

fn request_to_wit(request: WasmCompletionRequest) -> wit::CompletionRequest {
    wit::CompletionRequest {
        model: request.model,
        prompt: request.prompt,
        max_tokens: request.max_tokens,
        temperature: request.temperature,
    }
}

/// A human-readable, secret-free rendering of a WIT `provider-error`.
fn describe_provider_error(error: &wit::ProviderError) -> String {
    match error {
        wit::ProviderError::InvalidRequest(message) => {
            format!("invalid provider request: {message}")
        }
        wit::ProviderError::ModelNotFound => "provider model not found".to_string(),
        wit::ProviderError::RateLimited => "provider rate limited".to_string(),
        wit::ProviderError::Unavailable => "provider unavailable".to_string(),
        wit::ProviderError::Failed(message) => format!("provider failed: {message}"),
    }
}

/// Process-wide registry of live WASM provider transports, keyed by provider id.
/// The router's `anthropic_messages_stream` looks a routed connection up here by
/// `target.conn.provider` — a data-driven predicate, so the divert stays generic
/// (no plugin id string). Mirrors the leaked custom-descriptor cache in
/// `llm_router::registry`.
static WASM_PROVIDERS: OnceLock<RwLock<HashMap<String, Arc<dyn WasmProviderRuntime>>>> =
    OnceLock::new();

fn provider_registry() -> &'static RwLock<HashMap<String, Arc<dyn WasmProviderRuntime>>> {
    WASM_PROVIDERS.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Register (or replace) a live provider transport under its `provider_id`.
pub fn register_wasm_provider(transport: Arc<dyn WasmProviderRuntime>) {
    let id = transport.provider_id().to_string();
    provider_registry()
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(id, transport);
}

/// Look up a live provider transport by provider id — the router's generic
/// divert predicate. `None` means this connection is not backed by an installed
/// WASM provider bundle (so the generic HTTP path handles it).
pub fn wasm_provider(provider_id: &str) -> Option<Arc<dyn WasmProviderRuntime>> {
    provider_registry()
        .read()
        .unwrap_or_else(PoisonError::into_inner)
        .get(provider_id)
        .cloned()
}

/// Drop a provider transport from the registry (e.g. on uninstall/disable).
pub fn unregister_wasm_provider(provider_id: &str) {
    provider_registry()
        .write()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(provider_id);
}

/// The prebuilt provider fixture artifact (caller must build fixtures first).
/// Module-level (not inside `mod tests`) so the router-level test in
/// `llm_router::client` can reuse it.
#[cfg(test)]
pub(crate) fn provider_fixture_artifact() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/component-provider/target/wasm32-wasip2/release")
        .join("ryuzi_component_provider_fixture.wasm")
}

/// Build a [`WasmProviderTransport`] over the prebuilt provider fixture, keyed
/// by `provider_id`, under a `timeout`. Returns the store tempfile so it isn't
/// dropped before the transport is used. Shared with the router-level test in
/// `llm_router::client`, so it lives at module level rather than inside
/// `mod tests`.
#[cfg(test)]
pub(crate) async fn build_test_transport(
    component_path: std::path::PathBuf,
    provider_id: &str,
    timeout: std::time::Duration,
) -> (Arc<WasmProviderTransport>, tempfile::NamedTempFile) {
    use crate::plugins::bundle::InstalledBundle;
    use crate::plugins::runtime::{ComponentRuntime, HostPolicy};
    use crate::settings::SettingsStore;
    use crate::store::ComponentPluginReleaseRecord;
    use crate::telemetry::NoopTelemetry;
    use ryuzi_plugin_sdk::{
        PluginBundleManifest, PluginLifecycle, PluginPermissions, PluginRelease,
    };

    let mut policy = HostPolicy::deny_all();
    policy.limits.timeout = timeout;
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
    let ctx = Arc::new(PluginCapabilityContext {
        plugin_id: provider_id.to_string(),
        version: "0.1.0".to_string(),
        settings: SettingsStore::new(store.clone()),
        store,
        telemetry: Arc::new(NoopTelemetry),
        network_allowlist: vec![],
        oauth_profile_ids: vec![],
    });
    let bundle = InstalledBundle {
        manifest: PluginBundleManifest {
            id: provider_id.to_string(),
            name: provider_id.to_string(),
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
            id: provider_id.to_string(),
            version: "0.1.0".to_string(),
            wit_api: "0.1.0".to_string(),
            component_url: "https://example.invalid/x.wasm".to_string(),
            component_sha256: "0".repeat(64),
            size_bytes: None,
            published_at: None,
        },
        release_record: ComponentPluginReleaseRecord {
            plugin_id: provider_id.to_string(),
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
    };
    let runtime = ComponentRuntime::new().unwrap();
    let compiled = Arc::new(runtime.compile(&bundle, policy).unwrap());
    (
        Arc::new(WasmProviderTransport::new(
            compiled,
            ctx,
            provider_id.to_string(),
        )),
        tmp,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use crate::plugins::build_fixture_components_once as build_fixtures;

    fn provider_artifact() -> std::path::PathBuf {
        provider_fixture_artifact()
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn list_models_returns_the_static_fixture_model() {
        build_fixtures();
        let (transport, _tmp) = build_test_transport(
            provider_artifact(),
            "wasm-prov-list",
            Duration::from_secs(10),
        )
        .await;
        let models = transport
            .list_models()
            .await
            .expect("list-models must succeed");
        assert_eq!(models.len(), 1);
        assert_eq!(models[0].id, "fixture-model");
        assert_eq!(models[0].context_window, 8192);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn complete_returns_two_chunks_in_order() {
        build_fixtures();
        let (transport, _tmp) =
            build_test_transport(provider_artifact(), "wasm-prov-ok", Duration::from_secs(10))
                .await;
        let chunks = transport
            .complete(WasmCompletionRequest {
                model: "fixture-model".to_string(),
                prompt: "hello".to_string(),
                max_tokens: Some(64),
                temperature: Some(0.2),
            })
            .await
            .expect("complete must succeed");
        let texts: Vec<&str> = chunks.iter().map(|c| c.text.as_str()).collect();
        assert_eq!(
            texts,
            vec!["Hello, ", "world!"],
            "chunk order must be preserved"
        );
        assert!(!chunks[0].finished);
        assert!(chunks[1].finished);
        assert_eq!(
            chunks[1].usage,
            Some(WasmTokenUsage {
                input: 7,
                output: 3
            })
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn complete_surfaces_a_provider_error_without_crashing() {
        build_fixtures();
        let (transport, _tmp) = build_test_transport(
            provider_artifact(),
            "wasm-prov-reject",
            Duration::from_secs(10),
        )
        .await;
        let error = transport
            .complete(WasmCompletionRequest {
                model: "fixture-model".to_string(),
                prompt: "please reject".to_string(),
                max_tokens: None,
                temperature: None,
            })
            .await
            .expect_err("a provider-error must surface as Err");
        assert!(
            error.contains("intentional provider failure"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn complete_isolates_a_nonterminating_provider_via_timeout() {
        build_fixtures();
        let (transport, _tmp) = build_test_transport(
            provider_artifact(),
            "wasm-prov-boom",
            Duration::from_millis(200),
        )
        .await;
        let started = std::time::Instant::now();
        let error = transport
            .complete(WasmCompletionRequest {
                model: "fixture-model".to_string(),
                prompt: "make it boom".to_string(),
                max_tokens: None,
                temperature: None,
            })
            .await
            .expect_err("a looping completion must be caught, not hang the host");
        let elapsed = started.elapsed();
        assert!(
            error.contains("timeout") || error.contains("budget"),
            "expected a timeout error, got: {error}"
        );
        assert!(
            elapsed < Duration::from_secs(5),
            "timeout must fire promptly: {elapsed:?}"
        );
    }

    #[tokio::test]
    async fn register_and_lookup_round_trips_by_provider_id() {
        build_fixtures();
        let (transport, _tmp) = build_test_transport(
            provider_artifact(),
            "wasm-prov-registry",
            Duration::from_secs(10),
        )
        .await;
        assert!(wasm_provider("wasm-prov-registry").is_none());
        register_wasm_provider(transport);
        assert!(wasm_provider("wasm-prov-registry").is_some());
        unregister_wasm_provider("wasm-prov-registry");
        assert!(wasm_provider("wasm-prov-registry").is_none());
    }
}
