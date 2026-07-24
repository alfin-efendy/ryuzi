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
use crate::settings::SettingsStore;
use crate::store::Store;
use crate::telemetry::Telemetry;

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

/// Discover every active WASM component bundle under `root`, keep only the
/// ENABLED ones that export `ryuzi:provider/provider`, compile each once, and
/// register a live [`WasmProviderTransport`] into the process-wide
/// [`register_wasm_provider`] registry under EACH router provider id the bundle
/// declares (`PluginBundleManifest::resolved_provider_ids`, which falls back to
/// the bundle id when none are declared) — the provider analogue of
/// [`crate::plugins::wasm_gateway::discover_gateway_components`]. Returns the
/// provider ids registered (for logging / test cleanup); the daemon consumes no
/// value from it, since routing looks transports up out of the shared registry.
///
/// Every failure mode is warn-and-skip (missing root, discovery error,
/// unavailable runtime, per-bundle compile failure, enablement-lookup error),
/// so a broken provider plugin never blocks daemon startup, and a clean install
/// (nothing enabled that exports a provider) registers nothing. `root` is a
/// parameter (rather than always
/// [`crate::plugins::bundle::installed_bundle_root`]) purely so tests can point
/// discovery at a hermetic install root; production passes the real per-user
/// root.
pub(crate) async fn discover_provider_components(
    store: Arc<Store>,
    settings: &SettingsStore,
    telemetry: Arc<dyn Telemetry>,
    root: &std::path::Path,
) -> Vec<String> {
    use crate::plugins::runtime::{ComponentRuntime, HostPolicy};

    if !root.exists() {
        return Vec::new();
    }
    let bundles = match crate::plugins::bundle::load_active_bundles(root, &store).await {
        Ok(bundles) => bundles,
        Err(error) => {
            tracing::warn!("wasm provider: discovering component bundles failed: {error}");
            return Vec::new();
        }
    };
    if bundles.is_empty() {
        return Vec::new();
    }
    let runtime = match ComponentRuntime::new() {
        Ok(runtime) => runtime,
        Err(error) => {
            tracing::warn!("wasm provider: component runtime unavailable: {error}");
            return Vec::new();
        }
    };
    let mut registered = Vec::new();
    for bundle in bundles {
        let id = bundle.manifest.id.clone();
        match crate::plugins::host::component_plugin_enabled(settings, &id).await {
            Ok(true) => {}
            Ok(false) => continue,
            Err(error) => {
                tracing::warn!(plugin = %id, "wasm provider: enablement check failed: {error}");
                continue;
            }
        }
        // Single source of truth for the installed-bundle capability policy
        // (incl. the first-party-only `allow_self_auth` gate that keeps mimo's
        // bootstrap JWT header) — see `HostPolicy::for_installed_bundle`.
        let policy = HostPolicy::for_installed_bundle(&bundle);
        let compiled = match runtime.compile(&bundle, policy) {
            Ok(compiled) => Arc::new(compiled),
            Err(error) => {
                tracing::warn!(plugin = %id, "wasm provider: component compile failed: {error}");
                continue;
            }
        };
        // Only provider bundles are registered; a gateway/connector/hooks-only
        // bundle is skipped before any instantiation (IMP-2).
        if !compiled.exports_provider() {
            continue;
        }
        let ctx = Arc::new(PluginCapabilityContext {
            plugin_id: id.clone(),
            version: bundle.manifest.version.clone(),
            settings: settings.clone(),
            store: store.clone(),
            telemetry: telemetry.clone(),
            network_allowlist: bundle
                .manifest
                .permissions
                .network
                .iter()
                .map(|entry| entry.0.clone())
                .collect(),
            oauth_profile_ids: bundle
                .manifest
                .oauth
                .iter()
                .map(|profile| profile.id.clone())
                .collect(),
            provider_ids: bundle.manifest.resolved_provider_ids(),
        });
        // One transport per DECLARED router provider id (mimo -> `mimo-free`),
        // all sharing the single compiled component + capability context. The
        // bundle-id -> router-id mapping is data-driven from the manifest, so
        // there is NO plugin-id host branch here.
        for provider_id in bundle.manifest.resolved_provider_ids() {
            register_wasm_provider(Arc::new(WasmProviderTransport::new(
                compiled.clone(),
                ctx.clone(),
                provider_id.clone(),
            )));
            registered.push(provider_id);
        }
    }
    registered
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

/// The extra capability grants + storage seeding
/// [`build_test_transport_with_grants`] layers onto the baseline `deny_all`
/// policy every test transport starts from. Kept as one struct (rather than
/// growing the function's parameter list) so a future grant — e.g. a second
/// per-provider slice needing `ryuzi:oauth` — is one new field, not a new
/// call-site shape.
///
/// [`build_test_transport`] is the zero-grants case (`Self::default()`); the
/// provider-conformance harness
/// (`crate::plugins::wasm_provider_conformance`) drives both the http+storage
/// case (the synthetic fixture) and the http+storage+provider-auth case (the
/// real `openai` component) — all of them call THIS builder so the ~80 lines
/// of bundle/context/policy boilerplate exist exactly once.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct TestTransportGrants {
    /// Non-empty iff the http capability should be granted: allowlists these
    /// bare hosts (matched on host, not port) in both the manifest's
    /// `permissions.network` declaration (what authorizes the http import in
    /// the component linker) and `PluginCapabilityContext.network_allowlist`
    /// (what the host's `AllowedHttpClient` actually enforces at request
    /// time).
    pub network_allowlist: Vec<String>,
    /// Grants `ryuzi:storage`.
    pub allow_storage: bool,
    /// `(key, value)` pairs seeded into this provider's own storage slice
    /// before the transport is handed back — the generic endpoint-override
    /// channel a provider component reads through `ryuzi:storage` (e.g. the
    /// conformance harness's mock upstream base URL).
    pub storage_seed: Vec<(String, Vec<u8>)>,
    /// Router provider ids the bundle DECLARES (`provider-ids`). Non-empty
    /// alongside a non-empty `network_allowlist` grants
    /// `ryuzi:provider-auth` — the exact gate
    /// [`crate::plugins::runtime::HostPolicy::for_installed_bundle`] applies in
    /// production, mirrored here rather than re-derived, so a test transport
    /// can never be more permissive than a real install.
    pub provider_ids: Vec<String>,
    /// `(provider_id, api_key)` user credentials seeded through the SAME
    /// storage the real router uses (`llm_router::connections`, encrypted at
    /// rest by `llm_router::secrets`), so `ryuzi:provider-auth` resolves a real
    /// key instead of reporting `not-configured`.
    pub provider_credentials: Vec<(String, String)>,
    /// OAuth profile ids the bundle DECLARES (`[[oauth]]`). Non-empty grants
    /// `ryuzi:oauth` — the exact gate
    /// [`crate::plugins::runtime::HostPolicy::for_installed_bundle`] applies in
    /// production (`allow_oauth = !manifest.oauth.is_empty()`), mirrored here so
    /// a test transport can never be more permissive than a real install. Each
    /// id is also declared as an `[[oauth]]` profile on the test bundle
    /// manifest (what the compiled component reads to authorize the profile).
    pub oauth_profile_ids: Vec<String>,
    /// `(profile_id, access_token)` OAuth tokens seeded through the SAME store
    /// the real host reads (`Store::upsert_plugin_oauth_profile_token`, keyed by
    /// this bundle's plugin id), so `ryuzi:oauth`'s `authorized-request`
    /// resolves a real bearer to inject instead of reporting `denied`. The
    /// component never sees this value — the host injects it and returns only the
    /// upstream response, exactly as `capabilities::oauth`'s own tests seed it.
    pub oauth_tokens: Vec<(String, String)>,
}

/// Build a [`WasmProviderTransport`] over a component at `component_path`,
/// keyed by `provider_id`, under a `timeout`, with `grants` layered onto the
/// baseline `HostPolicy::deny_all()`. Returns the store tempfile so it isn't
/// dropped before the transport is used. Shared with the router-level test in
/// `llm_router::client` and the provider-conformance harness, so it lives at
/// module level rather than inside `mod tests`.
///
/// This deliberately builds the policy from `HostPolicy::deny_all()` directly
/// rather than the production `HostPolicy::for_installed_bundle` derivation —
/// every test transport's `release_record.signing_key_id` below is an inert
/// placeholder; `allow_self_auth` stays `false` (from `deny_all()`) no matter
/// what that field says, which is exactly what a strict-Authorization-
/// stripping check depends on.
#[cfg(test)]
pub(crate) async fn build_test_transport_with_grants(
    component_path: std::path::PathBuf,
    provider_id: &str,
    timeout: std::time::Duration,
    grants: TestTransportGrants,
) -> (Arc<WasmProviderTransport>, tempfile::NamedTempFile) {
    use crate::plugins::bundle::InstalledBundle;
    use crate::plugins::runtime::{ComponentRuntime, HostPolicy};
    use crate::settings::SettingsStore;
    use crate::store::ComponentPluginReleaseRecord;
    use crate::telemetry::NoopTelemetry;
    use ryuzi_plugin_sdk::{
        NetworkPermission, OAuthProfile, PluginBundleManifest, PluginLifecycle, PluginPermissions,
        PluginRelease,
    };

    let mut policy = HostPolicy::deny_all();
    policy.allow_network = !grants.network_allowlist.is_empty();
    policy.allow_storage = grants.allow_storage;
    // Same conjunction as `HostPolicy::for_installed_bundle`: an explicitly
    // declared provider id AND a declared outbound host.
    policy.allow_provider_auth =
        !grants.provider_ids.is_empty() && !grants.network_allowlist.is_empty();
    // Same gate as `HostPolicy::for_installed_bundle`: a declared `[[oauth]]`
    // profile grants `ryuzi:oauth`.
    policy.allow_oauth = !grants.oauth_profile_ids.is_empty();
    policy.limits.timeout = timeout;

    // Keeps the encrypted-at-rest credential seeding below off the real OS
    // keychain / `secret.key`. Guarded: this mutates the process-global
    // `RYUZI_SECRET_KEY_FILE`, so a transport that seeds NO credential must not
    // reach for it — a zero-grant transport has nothing to encrypt and no
    // business perturbing shared process state for every other test in the
    // binary.
    if !grants.provider_credentials.is_empty() {
        crate::llm_router::secrets::use_test_key_file();
    }
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
    for (key, value) in &grants.storage_seed {
        store
            .put_component_storage(provider_id, key, value)
            .await
            .unwrap();
    }
    for (credential_provider, api_key) in &grants.provider_credentials {
        let now = crate::paths::now_ms();
        crate::llm_router::connections::add_connection(
            &store,
            crate::llm_router::connections::ConnectionRow {
                id: format!("test-conn-{credential_provider}"),
                provider: credential_provider.clone(),
                auth_type: "api_key".to_string(),
                label: credential_provider.clone(),
                priority: 0,
                enabled: true,
                data: crate::llm_router::connections::ConnectionData {
                    api_key: Some(api_key.clone()),
                    ..Default::default()
                },
                created_at: now,
                updated_at: now,
            },
        )
        .await
        .unwrap();
    }
    // Seed OAuth profile tokens exactly the way `capabilities::oauth`'s own
    // tests do, keyed by this bundle's plugin id (== provider_id) and the
    // profile id — so the host resolves a real bearer to inject and the
    // conformance auth-absence check sees the HOST-injected value, not a guest
    // one. A comfortably future expiry keeps `needs_refresh` from reporting the
    // token expired mid-battery.
    for (profile_id, access_token) in &grants.oauth_tokens {
        let now = crate::paths::now_ms();
        store
            .upsert_plugin_oauth_profile_token(
                provider_id,
                profile_id,
                &crate::plugins::oauth::PluginOauthToken {
                    plugin_id: provider_id.to_string(),
                    access_token: access_token.clone(),
                    refresh_token: None,
                    token_type: "Bearer".to_string(),
                    expires_at: Some(now + 3_600_000),
                    scopes: vec![],
                    reconnect_required: false,
                },
            )
            .await
            .unwrap();
    }

    let ctx = Arc::new(PluginCapabilityContext {
        plugin_id: provider_id.to_string(),
        version: "0.1.0".to_string(),
        settings: SettingsStore::new(store.clone()),
        store,
        telemetry: Arc::new(NoopTelemetry),
        network_allowlist: grants.network_allowlist.clone(),
        oauth_profile_ids: grants.oauth_profile_ids.clone(),
        provider_ids: grants.provider_ids.clone(),
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
            permissions: PluginPermissions {
                network: grants
                    .network_allowlist
                    .iter()
                    .map(|host| NetworkPermission(host.clone()))
                    .collect(),
            },
            // One minimal `[[oauth]]` profile per granted id: the compiled
            // component reads these (`CompiledComponent.oauth_profile_ids`) to
            // authorize the profile the guest passes to `authorized-request`.
            // Only the id matters on the completion path — `authorized-request`
            // resolves the seeded token, not the authorize/token URLs.
            oauth: grants
                .oauth_profile_ids
                .iter()
                .map(|id| OAuthProfile {
                    id: id.clone(),
                    authorize_url: None,
                    token_url: None,
                    resource: None,
                    scopes: vec![],
                    client_id_setting: None,
                    client_secret_setting: None,
                    dynamic_registration: false,
                })
                .collect(),
            provider_ids: grants.provider_ids.clone(),
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
        // A placeholder, non-first-party signing key id. It plays no part in
        // `allow_self_auth` on this path (see the function doc above) — it
        // only exists to satisfy `InstalledBundle`'s shape.
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

/// Build a [`WasmProviderTransport`] over the prebuilt provider fixture, keyed
/// by `provider_id`, under a `timeout`, with NO extra capability grants (the
/// baseline case of [`build_test_transport_with_grants`]).
#[cfg(test)]
pub(crate) async fn build_test_transport(
    component_path: std::path::PathBuf,
    provider_id: &str,
    timeout: std::time::Duration,
) -> (Arc<WasmProviderTransport>, tempfile::NamedTempFile) {
    build_test_transport_with_grants(
        component_path,
        provider_id,
        timeout,
        TestTransportGrants::default(),
    )
    .await
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

    // -----------------------------------------------------------------
    // discover_provider_components: production discovery + registration
    // -----------------------------------------------------------------

    use crate::store::ComponentPluginReleaseRecord;
    use crate::telemetry::NoopTelemetry;

    fn gateway_fixture_artifact() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/component-gateway/target/wasm32-wasip2/release")
            .join("ryuzi_component_gateway_fixture.wasm")
    }

    /// Lay a verified, active bundle onto `root` in the exact on-disk layout
    /// [`crate::plugins::bundle::load_active_bundles`] requires (versioned dir +
    /// `current` pointer + `ryuzi-plugin.toml` + `release.json` + the component,
    /// hashes all agreeing) and seed the matching active release row into
    /// `store`. Signed under the first-party key so
    /// `HostPolicy::for_installed_bundle` grants `allow_self_auth`, exactly like
    /// the real mimo/opencode bundles.
    async fn install_bundle_on_disk(
        root: &std::path::Path,
        store: &Store,
        plugin_id: &str,
        component_artifact: &std::path::Path,
        provider_ids: &[&str],
    ) {
        use sha2::{Digest, Sha256};

        let version = "0.1.0";
        let component_name = "plugin.wasm";
        let version_dir = root.join(plugin_id).join(version);
        std::fs::create_dir_all(&version_dir).unwrap();
        let bytes = std::fs::read(component_artifact).unwrap();
        std::fs::write(version_dir.join(component_name), &bytes).unwrap();
        let sha = format!("{:x}", Sha256::digest(&bytes));

        let provider_ids_line = if provider_ids.is_empty() {
            String::new()
        } else {
            let quoted = provider_ids
                .iter()
                .map(|p| format!("\"{p}\""))
                .collect::<Vec<_>>()
                .join(", ");
            format!("provider-ids = [{quoted}]\n")
        };
        let manifest = format!(
            "id = \"{plugin_id}\"\n\
             name = \"{plugin_id}\"\n\
             version = \"{version}\"\n\
             wit-api = \"^0.1.0\"\n\
             lifecycle = \"per-call\"\n\
             component = \"{component_name}\"\n\
             {provider_ids_line}"
        );
        std::fs::write(version_dir.join("ryuzi-plugin.toml"), manifest).unwrap();

        let release = serde_json::json!({
            "id": plugin_id,
            "version": version,
            "wit-api": "0.1.0",
            "component_url": "https://example.invalid/x.wasm",
            "component_sha256": sha,
        });
        std::fs::write(
            version_dir.join("release.json"),
            serde_json::to_vec(&release).unwrap(),
        )
        .unwrap();
        std::fs::write(root.join(plugin_id).join("current"), version).unwrap();

        let record = ComponentPluginReleaseRecord {
            plugin_id: plugin_id.to_string(),
            version: version.to_string(),
            source_url: "https://example.invalid/x.wasm".to_string(),
            sha256: sha,
            signing_key_id: crate::plugins::first_party_key::FIRST_PARTY_KEY_ID.to_string(),
            installed_at: 0,
            active: false,
            revoked: false,
            revocation_reason: None,
        };
        store.upsert_component_release(&record).await.unwrap();
        store
            .set_active_component_release(plugin_id, version)
            .await
            .unwrap();
    }

    /// A fresh temp store + a `SettingsStore` over it + a throwaway on-disk
    /// install root, all sharing one lifetime tempfile so nothing is dropped
    /// mid-test.
    async fn discovery_env() -> (
        Arc<Store>,
        SettingsStore,
        tempfile::TempDir,
        tempfile::NamedTempFile,
    ) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let settings = SettingsStore::new(store.clone());
        let root = tempfile::tempdir().unwrap();
        (store, settings, root, tmp)
    }

    /// Flip a component plugin's enablement on. Writes the raw
    /// `plugin.<id>.enabled` row `component_plugin_enabled` reads (the schema
    /// `SettingsStore::set` path rejects a key for a plugin not registered in a
    /// `PluginHost`, which these hermetically installed on-disk bundles are not).
    async fn enable(store: &Store, plugin_id: &str) {
        store
            .set_setting_raw(&format!("plugin.{plugin_id}.enabled"), "true")
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn enabled_provider_bundle_registers_under_its_declared_id() {
        build_fixtures();
        let (store, settings, root, _tmp) = discovery_env().await;
        install_bundle_on_disk(
            root.path(),
            &store,
            "disc-prov-enabled",
            &provider_artifact(),
            &["disc-prov-enabled-served"],
        )
        .await;
        enable(&store, "disc-prov-enabled").await;

        let registered = super::discover_provider_components(
            store.clone(),
            &settings,
            Arc::new(NoopTelemetry),
            root.path(),
        )
        .await;

        assert_eq!(registered, vec!["disc-prov-enabled-served".to_string()]);
        let transport = wasm_provider("disc-prov-enabled-served")
            .expect("an enabled provider bundle must register a live transport");
        assert_eq!(transport.provider_id(), "disc-prov-enabled-served");

        for id in registered {
            unregister_wasm_provider(&id);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn declared_provider_ids_are_honored_and_empty_falls_back_to_manifest_id() {
        build_fixtures();
        let (store, settings, root, _tmp) = discovery_env().await;
        // A bundle whose router id (`disc-map-free`) differs from its bundle id
        // (`disc-map`) — the mimo/opencode shape.
        install_bundle_on_disk(
            root.path(),
            &store,
            "disc-map",
            &provider_artifact(),
            &["disc-map-free"],
        )
        .await;
        enable(&store, "disc-map").await;
        // A bundle that declares NO provider-ids: it must fall back to its id.
        install_bundle_on_disk(
            root.path(),
            &store,
            "disc-fallback",
            &provider_artifact(),
            &[],
        )
        .await;
        enable(&store, "disc-fallback").await;

        let registered = super::discover_provider_components(
            store.clone(),
            &settings,
            Arc::new(NoopTelemetry),
            root.path(),
        )
        .await;

        // Registered under the DECLARED router id, never the bundle id.
        assert!(
            wasm_provider("disc-map-free").is_some(),
            "must register under the declared router id",
        );
        assert!(
            wasm_provider("disc-map").is_none(),
            "must NOT register under the bundle id when provider-ids is declared",
        );
        // The no-declaration bundle falls back to its manifest id.
        assert!(
            wasm_provider("disc-fallback").is_some(),
            "an empty provider-ids must fall back to the manifest id",
        );

        for id in registered {
            unregister_wasm_provider(&id);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disabled_provider_bundle_registers_nothing() {
        build_fixtures();
        let (store, settings, root, _tmp) = discovery_env().await;
        // Installed + active but NOT enabled (component plugins default to
        // disabled — no auto-enable for providers).
        install_bundle_on_disk(
            root.path(),
            &store,
            "disc-disabled",
            &provider_artifact(),
            &["disc-disabled-served"],
        )
        .await;

        let registered = super::discover_provider_components(
            store.clone(),
            &settings,
            Arc::new(NoopTelemetry),
            root.path(),
        )
        .await;

        assert!(
            registered.is_empty(),
            "a disabled bundle must register nothing: {registered:?}",
        );
        assert!(wasm_provider("disc-disabled-served").is_none());
        assert!(wasm_provider("disc-disabled").is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn non_provider_bundle_registers_nothing() {
        build_fixtures();
        let (store, settings, root, _tmp) = discovery_env().await;
        // A gateway fixture: enabled + compiles, but exports gateway, not
        // provider — the `exports_provider()` gate must skip it.
        install_bundle_on_disk(
            root.path(),
            &store,
            "disc-gateway",
            &gateway_fixture_artifact(),
            &[],
        )
        .await;
        enable(&store, "disc-gateway").await;

        let registered = super::discover_provider_components(
            store.clone(),
            &settings,
            Arc::new(NoopTelemetry),
            root.path(),
        )
        .await;

        assert!(
            registered.is_empty(),
            "a non-provider (gateway) bundle must register nothing: {registered:?}",
        );
        assert!(wasm_provider("disc-gateway").is_none());
    }
}
