//! Component Model runtime validation.
//!
//! This first runtime slice intentionally grants no host capabilities. It
//! parses a component and rejects every import except a network import that is
//! declared in the bundle manifest and allowed by host policy. Capability
//! linking and component execution follow in later runtime slices.

use std::fmt;
use std::time::Duration;

use crate::plugins::bundle::{InstalledBundle, VerifiedBundle};
use crate::plugins::capabilities::host::HostInfo;
use crate::plugins::capabilities::http::{AllowedHttpClient, HttpErr};
use crate::plugins::capabilities::oauth::{OauthErr, ProfileOauth};
use crate::plugins::capabilities::provider_auth::{ProviderAuth, ProviderAuthErr};
use crate::plugins::capabilities::settings::{ScopedSettings, SettingsErr};
use crate::plugins::capabilities::storage::{PluginStorage, StorageErr};
use crate::plugins::capabilities::websocket::{WsConnState, WsErr, WsFrame, WsHeader, WsRegistry};
use crate::plugins::capabilities::wit_bindings::provider_auth::ryuzi::provider_auth::provider_auth as provider_auth_iface;
use crate::plugins::capabilities::wit_bindings::ryuzi::host::host as host_iface;
use crate::plugins::capabilities::wit_bindings::ryuzi::http::http as http_iface;
use crate::plugins::capabilities::wit_bindings::ryuzi::oauth::oauth as oauth_iface;
use crate::plugins::capabilities::wit_bindings::ryuzi::settings::settings as settings_iface;
use crate::plugins::capabilities::wit_bindings::ryuzi::storage::storage as storage_iface;
use crate::plugins::capabilities::wit_bindings::websocket::ryuzi::websocket::websocket as websocket_iface;
use crate::plugins::capabilities::PluginCapabilityContext;
use ryuzi_plugin_sdk::PluginBundleManifest;
use std::sync::Arc;
use wasmtime::{
    component::{Component, HasSelf, Instance, Linker},
    Config, Engine, Store,
};

const HTTP_IMPORT: &str = "ryuzi:http/http@0.1.0";
const SETTINGS_IMPORT: &str = "ryuzi:settings/settings@0.1.0";
const STORAGE_IMPORT: &str = "ryuzi:storage/storage@0.1.0";
const HOST_IMPORT: &str = "ryuzi:host/host@0.1.1";
const OAUTH_IMPORT: &str = "ryuzi:oauth/oauth@0.2.0";
/// The `ryuzi:websocket/websocket` host-import interface id. Linked by this
/// FULLY-QUALIFIED name (never a short `"websocket"` instance name) so a
/// component's import — always keyed by the fully-qualified id — actually
/// matches the adapter (the Task-13b regression).
const WEBSOCKET_IMPORT: &str = "ryuzi:websocket/websocket@0.1.0";
/// The `ryuzi:provider-auth/provider-auth` host-import interface id (Task
/// 16c1). Like `ryuzi:websocket`, this is an ADDITIVE capability package that
/// the root `ryuzi:plugin@0.1.0` world deliberately does not import, so every
/// already-installed component's `wit-api` range keeps resolving unchanged;
/// only a bundle that imports it (and is granted it) links it.
const PROVIDER_AUTH_IMPORT: &str = "ryuzi:provider-auth/provider-auth@0.1.0";
const TYPES_IMPORT: &str = "ryuzi:plugin/types@0.1.0";
const LIFECYCLE_EXPORT: &str = "ryuzi:plugin/lifecycle@0.1.0";
/// The `ryuzi:connector/connector` export interface name — the single source
/// of truth shared by `ALLOWED_EXPORTS` and [`CompiledComponent::exports_connector`].
pub(crate) const CONNECTOR_EXPORT: &str = "ryuzi:connector/connector@0.1.0";
/// The `ryuzi:hooks/hooks` export interface name — shared by `ALLOWED_EXPORTS`
/// and [`CompiledComponent::exports_hooks`].
pub(crate) const HOOKS_EXPORT: &str = "ryuzi:hooks/hooks@0.1.0";
/// The `ryuzi:provider/provider` export interface name — shared by
/// `ALLOWED_EXPORTS` and [`CompiledComponent::exports_provider`] (Task 10).
pub(crate) const PROVIDER_EXPORT: &str = "ryuzi:provider/provider@0.1.0";
/// The `ryuzi:gateway/gateway` export interface name — shared by
/// `ALLOWED_EXPORTS` and [`CompiledComponent::exports_gateway`] (Task 10).
pub(crate) const GATEWAY_EXPORT: &str = "ryuzi:gateway/gateway@0.1.0";
const ALLOWED_EXPORTS: &[&str] = &[
    "lifecycle",
    LIFECYCLE_EXPORT,
    GATEWAY_EXPORT,
    CONNECTOR_EXPORT,
    PROVIDER_EXPORT,
    HOOKS_EXPORT,
];

/// Default resource budget a plugin runtime may consume.
///
/// **Enforcement note:** `max_memory_bytes` and `max_concurrency` are declared
/// here so callers can configure them ahead of time, but their runtime
/// enforcement intentionally arrives with the later supervision / capability
/// slice.  The current slice validates structure only; no guarantee is made
/// that these limits are applied during component execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimits {
    /// Hard memory ceiling in bytes.
    ///
    /// Not yet enforced — enforcement will be introduced together with the
    /// runtime supervision slice.
    pub max_memory_bytes: u64,
    pub fuel: u64,
    pub timeout: Duration,
    /// Maximum concurrent tasks a component may spawn.
    ///
    /// Not yet enforced — enforcement will be introduced together with the
    /// runtime supervision slice.
    pub max_concurrency: usize,
}

impl Default for ResourceLimits {
    fn default() -> Self {
        Self {
            max_memory_bytes: 64 * 1024 * 1024,
            fuel: 10_000_000,
            timeout: Duration::from_secs(30),
            max_concurrency: 4,
        }
    }
}

/// Capabilities a particular component activation may use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostPolicy {
    pub allow_network: bool,
    /// Grants `ryuzi:settings/settings` — a plugin's own scoped
    /// `plugin.<id>.*` settings slice (see
    /// `capabilities::settings`'s module doc for the scoping guarantee).
    pub allow_settings: bool,
    /// Grants `ryuzi:storage/storage` — a plugin's own scoped rows in
    /// `component_plugin_storage`.
    pub allow_storage: bool,
    /// Grants host-mediated `ryuzi:oauth/oauth@0.2.0`; components can make
    /// authorized profile requests but never receive raw OAuth tokens.
    pub allow_oauth: bool,
    /// Grants host-mediated `ryuzi:websocket/websocket@0.1.0`; the host owns
    /// the raw TLS WebSocket and the component drives it. Gated on the same
    /// non-empty-network manifest declaration as `allow_network` (a component
    /// that declares no network host can open no WebSocket).
    pub allow_websocket: bool,
    /// Grants host-mediated `ryuzi:provider-auth/provider-auth@0.1.0`: the
    /// component can issue a request authenticated with the user's STORED
    /// provider API key for a provider id its manifest declares, without ever
    /// receiving the key itself (see `capabilities::provider_auth`).
    pub allow_provider_auth: bool,
    /// Lets this bundle set its OWN `Authorization` header on `ryuzi:http/http`
    /// requests (the initial hop to an allowlisted host only). Granted ONLY to
    /// VERIFIED first-party bundles — the caller derives it from the installed
    /// release's `signing_key_id == first_party_key::FIRST_PARTY_KEY_ID`, NEVER
    /// from manifest content or anything a component can forge. Every ordinary
    /// bundle keeps the strict Task 8 stripping (see `capabilities::http`). The
    /// self-set bearer is dropped on every redirect and never coexists with a
    /// host-injected OAuth bearer.
    pub allow_self_auth: bool,
    pub limits: ResourceLimits,
}

impl HostPolicy {
    /// The default policy: no component receives host capabilities.
    pub fn deny_all() -> Self {
        Self {
            allow_network: false,
            allow_settings: false,
            allow_storage: false,
            allow_oauth: false,
            allow_websocket: false,
            allow_provider_auth: false,
            allow_self_auth: false,
            limits: ResourceLimits::default(),
        }
    }

    /// The standard capability policy for an installed, active component bundle
    /// — the SINGLE source of truth every `InstalledBundle → HostPolicy` site
    /// must use (session connector/hooks activation, the gateway supervisor,
    /// and any future provider-transport wiring), so a security-sensitive flag
    /// can never be silently omitted or mis-derived by an inline copy.
    ///
    /// Capabilities are granted from the bundle's own manifest declarations plus
    /// its verified install provenance:
    /// - `allow_network` only when the manifest declares hosts, `allow_oauth`
    ///   only when it declares profiles;
    /// - `allow_settings`/`allow_storage` are always granted — a plugin's own
    ///   scoped settings/storage are safe by construction, and real outbound
    ///   network stays gated by the host-mediated `ryuzi:http`/`ryuzi:oauth`
    ///   capabilities;
    /// - `allow_provider_auth` only when the manifest EXPLICITLY declares
    ///   `provider-ids` AND declares a network host;
    /// - `allow_self_auth` (let the bundle set its own `Authorization` header)
    ///   ONLY for a VERIFIED first-party bundle, keyed off the installed
    ///   release's `signing_key_id == first_party_key::FIRST_PARTY_KEY_ID` — set
    ///   by `verify_bundle` from the trusted-key match, NEVER from manifest
    ///   content or anything a component can forge. Every other bundle keeps the
    ///   strict Task 8 `Authorization` stripping.
    pub fn for_installed_bundle(bundle: &InstalledBundle) -> Self {
        Self {
            allow_network: !bundle.manifest.permissions.network.is_empty(),
            allow_settings: true,
            allow_storage: true,
            allow_oauth: !bundle.manifest.oauth.is_empty(),
            // Same non-empty-network gate as `allow_network`: a bundle that
            // declares no network host is granted no WebSocket capability.
            allow_websocket: !bundle.manifest.permissions.network.is_empty(),
            // Fail-closed and EXPLICIT: the bundle must both declare the router
            // provider ids it serves (`provider-ids` — the `[id]` fallback of
            // `resolved_provider_ids` is for transport registration, never for
            // credential authorization) and declare at least one outbound host,
            // since an injected credential is only ever useful on a real
            // request and `AllowedHttpClient` refuses every host otherwise.
            allow_provider_auth: !bundle.manifest.provider_ids.is_empty()
                && !bundle.manifest.permissions.network.is_empty(),
            allow_self_auth: bundle.release_record.signing_key_id
                == crate::plugins::first_party_key::FIRST_PARTY_KEY_ID,
            limits: ResourceLimits::default(),
        }
    }
}

#[derive(Debug)]
pub enum PluginRuntimeError {
    EngineInitialization(String),
    ComponentRead(String),
    MalformedComponent(String),
    DeniedImport { name: String, reason: String },
    DeniedExport { name: String, reason: String },
    InstantiationFailed(String),
    FuelExhausted(String),
    TimeoutExceeded { timeout: Duration },
}

impl fmt::Display for PluginRuntimeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EngineInitialization(message) => {
                write!(f, "component engine failed to initialize: {message}")
            }
            Self::ComponentRead(message) => write!(f, "failed to read component: {message}"),
            Self::MalformedComponent(message) => write!(f, "malformed component: {message}"),
            Self::DeniedImport { name, reason } => {
                write!(f, "component import `{name}` is denied: {reason}")
            }
            Self::DeniedExport { name, reason } => {
                write!(f, "component export `{name}` is denied: {reason}")
            }
            Self::InstantiationFailed(message) => {
                write!(f, "component instantiation failed: {message}")
            }
            Self::FuelExhausted(message) => {
                write!(f, "component exhausted its fuel budget: {message}")
            }
            Self::TimeoutExceeded { timeout } => {
                write!(f, "component exceeded its timeout budget of {timeout:?}")
            }
        }
    }
}

impl std::error::Error for PluginRuntimeError {}

/// Build a component-model [`Engine`] with fuel + epoch interruption + async
/// support. Called once for the runtime's own validation engine and once PER
/// [`CompiledComponent`] in [`ComponentRuntime::compile`], so each installed
/// component owns an independent epoch counter (IMP-1): one component hitting
/// its timeout (`engine.increment_epoch()`) can never trip another
/// concurrently-executing component's epoch deadline.
fn build_component_engine() -> Result<Engine, PluginRuntimeError> {
    let mut config = Config::new();
    config.wasm_component_model(true);
    config.consume_fuel(true);
    config.epoch_interruption(true);
    // Wasmtime 46 always enables async support, but task 6 requires this
    // explicit configuration call. Narrow suppression keeps clippy clean
    // without widening to module or crate scope.
    #[allow(deprecated)]
    config.async_support(true);
    Engine::new(&config)
        .map_err(|error| PluginRuntimeError::EngineInitialization(error.to_string()))
}

/// Validate a compiled component's imports and exports against its manifest +
/// host policy, returning the set of exported interface names on success (for
/// [`CompiledComponent`] to later skip probing interfaces a component does not
/// export — IMP-2). `component` must have been compiled against `engine`.
fn validate_component_interfaces(
    engine: &Engine,
    manifest: &PluginBundleManifest,
    component: &Component,
    policy: &HostPolicy,
) -> Result<Vec<String>, PluginRuntimeError> {
    for (name, _) in component.component_type().imports(engine) {
        let is_wasi_baseline = name.starts_with("wasi:");
        let network_is_authorized =
            name == HTTP_IMPORT && !manifest.permissions.network.is_empty() && policy.allow_network;
        let types_is_authorized = name == TYPES_IMPORT;
        let host_is_authorized = name == HOST_IMPORT;
        let settings_is_authorized = name == SETTINGS_IMPORT && policy.allow_settings;
        let storage_is_authorized = name == STORAGE_IMPORT && policy.allow_storage;
        let oauth_is_authorized = name == OAUTH_IMPORT && policy.allow_oauth;
        let websocket_is_authorized = name == WEBSOCKET_IMPORT && policy.allow_websocket;
        let provider_auth_is_authorized =
            name == PROVIDER_AUTH_IMPORT && policy.allow_provider_auth;
        if !is_wasi_baseline
            && !types_is_authorized
            && !network_is_authorized
            && !host_is_authorized
            && !settings_is_authorized
            && !storage_is_authorized
            && !oauth_is_authorized
            && !websocket_is_authorized
            && !provider_auth_is_authorized
        {
            let reason = if name == HTTP_IMPORT {
                "network requires a manifest allowlist and host policy approval".to_string()
            } else if name == SETTINGS_IMPORT {
                "settings access requires host policy approval".to_string()
            } else if name == STORAGE_IMPORT {
                "storage access requires host policy approval".to_string()
            } else if name == OAUTH_IMPORT {
                "OAuth access requires host policy approval".to_string()
            } else if name == WEBSOCKET_IMPORT {
                "WebSocket access requires a manifest network allowlist and host policy approval"
                    .to_string()
            } else if name == PROVIDER_AUTH_IMPORT {
                "provider credential injection requires declared manifest `provider-ids`, \
                 a manifest network allowlist, and host policy approval"
                    .to_string()
            } else {
                "no host capability is enabled by this runtime slice".to_string()
            };
            return Err(PluginRuntimeError::DeniedImport {
                name: name.to_string(),
                reason,
            });
        }
    }
    let mut exports = Vec::new();
    for (name, _) in component.component_type().exports(engine) {
        if !ALLOWED_EXPORTS.contains(&name) {
            return Err(PluginRuntimeError::DeniedExport {
                name: name.to_string(),
                reason: "not declared by the ryuzi:plugin@0.1.0 world".to_string(),
            });
        }
        exports.push(name.to_string());
    }
    Ok(exports)
}

/// Validates a WebAssembly component before later runtime layers link it.
pub struct ComponentRuntime {
    engine: Engine,
}

impl ComponentRuntime {
    pub fn new() -> Result<Self, PluginRuntimeError> {
        Ok(Self {
            engine: build_component_engine()?,
        })
    }

    /// Compile `bytes` against `self.engine` and validate its imports/exports,
    /// discarding the export set. Retained for `validate_component` and the
    /// runtime's own tests. `compile` does NOT go through here — it needs the
    /// component compiled against its OWN isolated engine (see
    /// [`Self::compile`]).
    fn validate_component_bytes(
        &self,
        manifest: &PluginBundleManifest,
        bytes: &[u8],
        policy: &HostPolicy,
    ) -> Result<Component, PluginRuntimeError> {
        let component = Component::new(&self.engine, bytes)
            .map_err(|error| PluginRuntimeError::MalformedComponent(error.to_string()))?;
        validate_component_interfaces(&self.engine, manifest, &component, policy)?;
        Ok(component)
    }

    /// Validates the component staged by a signed bundle under deny-all policy.
    pub fn validate_component(&self, bundle: &VerifiedBundle) -> Result<(), PluginRuntimeError> {
        let bytes = std::fs::read(bundle.staging_dir.join(&bundle.manifest.component))
            .map_err(|error| PluginRuntimeError::ComponentRead(error.to_string()))?;
        self.validate_component_bytes(&bundle.manifest, &bytes, &HostPolicy::deny_all())
            .map(|_| ())
    }

    /// Validates and compiles a bundle's component under `policy`, returning a
    /// reusable [`CompiledComponent`]. Compilation (the expensive step —
    /// parsing + JIT) happens once here; [`CompiledComponent::instantiate`] is
    /// comparatively cheap and safe to call per operation, so a connector tool
    /// invoked repeatedly re-instantiates a fresh, isolated instance without
    /// recompiling. Shared foundation for the connector/hooks adapters (Task 9)
    /// and the provider/gateway adapters (Task 10).
    pub fn compile(
        &self,
        bundle: &InstalledBundle,
        policy: HostPolicy,
    ) -> Result<CompiledComponent, PluginRuntimeError> {
        let bytes = std::fs::read(&bundle.component_path)
            .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
        // IMP-1: each component is compiled against its OWN engine so its epoch
        // counter is isolated from every other enabled component. A `Component`
        // is tied to the `Engine` it is compiled with, so the engine, the
        // component, and every `Store`/`Linker` built from it in
        // `CompiledComponent::instantiate` all share this one private engine —
        // and `ComponentInstance::call`'s timeout `increment_epoch()` only ever
        // advances THIS component's epoch.
        let engine = build_component_engine()?;
        let component = Component::new(&engine, &bytes)
            .map_err(|error| PluginRuntimeError::MalformedComponent(error.to_string()))?;
        let exports =
            validate_component_interfaces(&engine, &bundle.manifest, &component, &policy)?;
        // Built from the manifest's own declared network permissions (not
        // policy-conditioned here — the import is only linked at all when
        // `allow_network` is true, and validation already requires a non-empty
        // manifest allowlist for the import to be authorized in the first
        // place).
        let network_allowlist: Vec<String> = bundle
            .manifest
            .permissions
            .network
            .iter()
            .map(|entry| entry.0.clone())
            .collect();
        let oauth_profile_ids: Vec<String> = bundle
            .manifest
            .oauth
            .iter()
            .map(|profile| profile.id.clone())
            .collect();
        // The router provider ids this bundle is authorized to borrow a stored
        // user API key for. ONE rule governs that authorization: the EXPLICIT
        // manifest `provider-ids`, read from the same field
        // `HostPolicy::allow_provider_auth` gates the capability grant on. The
        // `[id]` fallback of `resolved_provider_ids` exists for transport
        // registration and must never widen a credential grant, so it is
        // deliberately not used here.
        let provider_ids = bundle.manifest.provider_ids.clone();
        Ok(CompiledComponent {
            engine,
            component,
            policy,
            plugin_id: bundle.manifest.id.clone(),
            version: bundle.manifest.version.clone(),
            network_allowlist,
            oauth_profile_ids,
            provider_ids,
            exports,
        })
    }

    /// Instantiates a component after policy validation, discarding the
    /// resulting instance. Retained as the original one-shot entrypoint
    /// (`compile` + `instantiate` + discard) so existing callers/tests that
    /// only need to prove a component links and runs `start` under the
    /// fuel/epoch budget keep working unchanged. New callers that need to CALL
    /// a component's exports use [`Self::compile`] +
    /// [`CompiledComponent::instantiate`] and keep the returned
    /// [`ComponentInstance`].
    pub async fn instantiate(
        &self,
        bundle: &InstalledBundle,
        policy: HostPolicy,
        ctx: Arc<PluginCapabilityContext>,
    ) -> Result<(), PluginRuntimeError> {
        self.compile(bundle, policy)?
            .instantiate(ctx)
            .await
            .map(|_instance| ())
    }

    #[cfg(test)]
    fn execute_core_module_with_fuel(
        &self,
        wat: &str,
        fuel: u64,
    ) -> Result<(), PluginRuntimeError> {
        let module = wasmtime::Module::new(&self.engine, wat)
            .map_err(|error| PluginRuntimeError::MalformedComponent(error.to_string()))?;
        let mut store = Store::new(&self.engine, ());
        store
            .set_fuel(fuel)
            .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
        store.set_epoch_deadline(1);
        let linker = wasmtime::Linker::<()>::new(&self.engine);
        linker
            .instantiate(&mut store, &module)
            .map(|_| ())
            .map_err(|error| match error.downcast_ref::<wasmtime::Trap>() {
                Some(wasmtime::Trap::OutOfFuel) => {
                    PluginRuntimeError::FuelExhausted(error.to_string())
                }
                _ => PluginRuntimeError::InstantiationFailed(error.to_string()),
            })
    }
}

/// Classify a `wasmtime::Error` raised by instantiation or an export call: an
/// out-of-fuel trap becomes [`PluginRuntimeError::FuelExhausted`]; anything
/// else (including an epoch-interrupt trap — which the timeout race relabels
/// as [`PluginRuntimeError::TimeoutExceeded`] before it is ever surfaced)
/// becomes [`PluginRuntimeError::InstantiationFailed`].
fn map_component_error(error: wasmtime::Error) -> PluginRuntimeError {
    match error.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::OutOfFuel) => PluginRuntimeError::FuelExhausted(error.to_string()),
        _ => PluginRuntimeError::InstantiationFailed(error.to_string()),
    }
}

/// A validated, compiled component held ready to instantiate on demand.
///
/// The component's imports/exports were already checked against its manifest
/// and host policy by [`ComponentRuntime::compile`]; the expensive
/// parse+compile has happened once. Each [`Self::instantiate`] then produces a
/// fresh, independent [`ComponentInstance`] with its own `Store`, so instances
/// never share mutable Wasm state and are safe to build per operation (e.g. a
/// stateless connector tool re-instantiates for every invoke) and use
/// concurrently. This is the reusable seam Task 10 builds provider/gateway
/// activation on top of.
pub struct CompiledComponent {
    engine: Engine,
    component: Component,
    policy: HostPolicy,
    plugin_id: String,
    version: String,
    network_allowlist: Vec<String>,
    oauth_profile_ids: Vec<String>,
    /// The router provider ids this bundle declared it serves — the ONLY
    /// providers whose stored user API key the `ryuzi:provider-auth` adapter
    /// will inject on its behalf (see `capabilities::provider_auth`).
    provider_ids: Vec<String>,
    /// The component's exported interface names (a subset of `ALLOWED_EXPORTS`),
    /// captured at compile time so an adapter can skip instantiating/probing a
    /// component that does not export the interface it wants (IMP-2).
    exports: Vec<String>,
}

impl CompiledComponent {
    /// The compiled plugin's id (its bundle manifest id).
    pub fn plugin_id(&self) -> &str {
        &self.plugin_id
    }

    /// Whether this component exports `ryuzi:connector/connector` — used by the
    /// connector tool enumeration to skip components that provide no tools
    /// (e.g. a hooks-only plugin) without instantiating them (IMP-2).
    pub(crate) fn exports_connector(&self) -> bool {
        self.exports.iter().any(|name| name == CONNECTOR_EXPORT)
    }

    /// Whether this component exports `ryuzi:hooks/hooks` — used by the hook
    /// dispatcher to skip components with no hooks (e.g. a connector-only
    /// plugin) without instantiating them or logging a warning (IMP-2).
    pub(crate) fn exports_hooks(&self) -> bool {
        self.exports.iter().any(|name| name == HOOKS_EXPORT)
    }

    /// Whether this component exports `ryuzi:provider/provider` — used by the
    /// generic provider transport to skip a non-provider bundle before any
    /// instantiation (Task 10, mirrors the IMP-2 connector/hooks gating).
    pub(crate) fn exports_provider(&self) -> bool {
        self.exports.iter().any(|name| name == PROVIDER_EXPORT)
    }

    /// Whether this component exports `ryuzi:gateway/gateway` — used by the
    /// daemon's gateway-supervisor discovery to skip a non-gateway bundle
    /// before any instantiation (Task 10, mirrors the IMP-2 gating).
    pub(crate) fn exports_gateway(&self) -> bool {
        self.exports.iter().any(|name| name == GATEWAY_EXPORT)
    }

    /// Instantiate a fresh, isolated instance, linking the host capability
    /// adapters (`ryuzi:host/host` always; `ryuzi:settings/settings`,
    /// `ryuzi:storage/storage`, `ryuzi:http/http`, `ryuzi:oauth/oauth` only
    /// when `policy` grants them) into the linker, then running the
    /// component's `start` under the fuel + epoch-timeout budget.
    ///
    /// `ctx` carries only the shared settings/store/telemetry backends; this
    /// instance's plugin identity, network allowlist, and OAuth profile ids
    /// come from the compiled bundle (see [`ComponentRuntime::compile`]),
    /// never from the caller.
    ///
    /// Timeout enforcement uses epoch interruption on a blocking thread:
    /// synchronous `Linker::instantiate` runs inside `spawn_blocking`, and a
    /// `tokio::select!` races it against the deadline. When the timer fires
    /// first we call `engine.increment_epoch()` once so the CPU-bound Wasm
    /// sees its epoch deadline and traps with an interrupt. The join handle is
    /// never detached — we always await it.
    pub async fn instantiate(
        &self,
        ctx: Arc<PluginCapabilityContext>,
    ) -> Result<ComponentInstance, PluginRuntimeError> {
        let engine = self.engine.clone();
        let component = self.component.clone();
        let fuel = self.policy.limits.fuel;
        let timeout = self.policy.limits.timeout;
        let allow_network = self.policy.allow_network;
        let allow_settings = self.policy.allow_settings;
        let allow_storage = self.policy.allow_storage;
        let allow_oauth = self.policy.allow_oauth;
        let allow_websocket = self.policy.allow_websocket;
        let allow_provider_auth = self.policy.allow_provider_auth;
        let allow_self_auth = self.policy.allow_self_auth;
        let network_allowlist = self.network_allowlist.clone();
        let runtime_ctx = Arc::new(PluginCapabilityContext {
            plugin_id: self.plugin_id.clone(),
            version: self.version.clone(),
            settings: ctx.settings.clone(),
            store: ctx.store.clone(),
            telemetry: ctx.telemetry.clone(),
            network_allowlist: network_allowlist.clone(),
            oauth_profile_ids: self.oauth_profile_ids.clone(),
            provider_ids: self.provider_ids.clone(),
        });
        // Captured on the async caller's thread — inside `spawn_blocking`
        // there is no ambient Tokio reactor, so the sync `Host` trait impls
        // bridge back to it explicitly via `Handle::block_on`.
        let rt = tokio::runtime::Handle::current();

        // Run synchronous instantiation on a blocking thread so that
        // tokio::select! can race it against a sleep timer. A plain
        // tokio::time::timeout around await cannot preempt CPU-bound Wasm;
        // epoch interruption on a blocking thread is the correct mechanism.
        let join_handle = tokio::task::spawn_blocking(move || {
            let state = CapabilityState {
                ctx: runtime_ctx,
                allow_network,
                network_allowlist,
                allow_self_auth,
                http_timeout: timeout,
                rt,
                // Locked down: no preopens, no env, no stdio, no sockets.
                wasi_ctx: wasmtime_wasi::WasiCtxBuilder::new().build(),
                wasi_table: wasmtime::component::ResourceTable::new(),
                ws: WsRegistry::new(),
            };
            let mut store = Store::new(&engine, state);
            store
                .set_fuel(fuel)
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            store.set_epoch_deadline(1);
            let mut linker = Linker::new(&engine);
            // The WASI p2 baseline: any std-built component imports it even
            // when it performs no I/O, so it must be linked for instantiation
            // to succeed. The empty `WasiCtx` above grants no real capability.
            wasmtime_wasi::p2::add_to_linker_sync(&mut linker)
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            // `host` carries no secrets and has no side effects — always
            // linked regardless of policy.
            host_iface::add_to_linker_instance::<CapabilityState, HasSelf<CapabilityState>>(
                &mut linker
                    .instance(HOST_IMPORT)
                    .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?,
                |s: &mut CapabilityState| s,
            )
            .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            if allow_settings {
                settings_iface::add_to_linker_instance::<CapabilityState, HasSelf<CapabilityState>>(
                    &mut linker.instance(SETTINGS_IMPORT).map_err(|error| {
                        PluginRuntimeError::InstantiationFailed(error.to_string())
                    })?,
                    |s: &mut CapabilityState| s,
                )
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            }
            if allow_storage {
                storage_iface::add_to_linker_instance::<CapabilityState, HasSelf<CapabilityState>>(
                    &mut linker.instance(STORAGE_IMPORT).map_err(|error| {
                        PluginRuntimeError::InstantiationFailed(error.to_string())
                    })?,
                    |s: &mut CapabilityState| s,
                )
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            }
            if allow_network {
                http_iface::add_to_linker_instance::<CapabilityState, HasSelf<CapabilityState>>(
                    &mut linker.instance(HTTP_IMPORT).map_err(|error| {
                        PluginRuntimeError::InstantiationFailed(error.to_string())
                    })?,
                    |s: &mut CapabilityState| s,
                )
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            }
            if allow_oauth {
                oauth_iface::add_to_linker_instance::<CapabilityState, HasSelf<CapabilityState>>(
                    &mut linker.instance(OAUTH_IMPORT).map_err(|error| {
                        PluginRuntimeError::InstantiationFailed(error.to_string())
                    })?,
                    |s: &mut CapabilityState| s,
                )
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            }
            if allow_websocket {
                // Linked by the FULLY-QUALIFIED interface id (WEBSOCKET_IMPORT),
                // never a short `"websocket"` instance name — a component's
                // import is keyed by the full id, so a short name would never
                // match (the Task-13b regression, guarded by
                // `instantiate_links_the_websocket_capability_by_full_interface_id`).
                websocket_iface::add_to_linker_instance::<CapabilityState, HasSelf<CapabilityState>>(
                    &mut linker.instance(WEBSOCKET_IMPORT).map_err(|error| {
                        PluginRuntimeError::InstantiationFailed(error.to_string())
                    })?,
                    |s: &mut CapabilityState| s,
                )
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            }
            if allow_provider_auth {
                // Linked by the FULLY-QUALIFIED interface id, like every other
                // capability above (the Task-13b regression guard).
                provider_auth_iface::add_to_linker_instance::<
                    CapabilityState,
                    HasSelf<CapabilityState>,
                >(
                    &mut linker.instance(PROVIDER_AUTH_IMPORT).map_err(|error| {
                        PluginRuntimeError::InstantiationFailed(error.to_string())
                    })?,
                    |s: &mut CapabilityState| s,
                )
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            }
            let instance = linker
                .instantiate(&mut store, &component)
                .map_err(map_component_error)?;
            Ok::<_, PluginRuntimeError>((instance, store))
        });

        tokio::pin!(join_handle);

        tokio::select! {
            result = &mut join_handle => match result {
                Ok(Ok((instance, store))) => Ok(ComponentInstance {
                    inner: Some((instance, store)),
                    engine: self.engine.clone(),
                    fuel,
                    timeout,
                }),
                Ok(Err(error)) => Err(error),
                Err(join_error) => Err(PluginRuntimeError::InstantiationFailed(format!(
                    "instantiation task panicked: {join_error}"
                ))),
            },
            _ = tokio::time::sleep(timeout) => {
                // The timer fired first. Increment the epoch exactly once so
                // the CPU-bound Wasm sees its epoch deadline and traps with an
                // interrupt.
                self.engine.increment_epoch();
                // Await the blocking task so we never detach a background
                // thread; whatever it produced after the deadline is mapped to
                // TimeoutExceeded because the operation exceeded its budget.
                match join_handle.await {
                    Ok(_) => Err(PluginRuntimeError::TimeoutExceeded { timeout }),
                    Err(join_error) => Err(PluginRuntimeError::InstantiationFailed(format!(
                        "instantiation task panicked: {join_error}"
                    ))),
                }
            }
        }
    }
}

/// A single live component instance and its `Store`, ready to have any of its
/// exported interfaces called under a per-call fuel + epoch-timeout budget.
///
/// [`Self::call`] is deliberately generic over which export it invokes: it
/// hands a caller-supplied closure the raw `Instance` and its `&mut Store`
/// inside the isolation budget, so the connector/hooks adapters (Task 9) and
/// the provider/gateway adapters (Task 10) all reach their exports through the
/// SAME path with no per-interface special-casing here. A caller reaches a
/// specific export via the generated per-interface accessor, e.g.
/// `exports::ryuzi::connector::connector::Guest::new(&mut *store, instance)`,
/// which only requires THAT interface to be exported — so a component that
/// exports a subset of the `ryuzi:plugin` world (a hooks-only plugin, a
/// gateway-only plugin, …) is handled uniformly, and asking for an interface
/// it does not export surfaces as a clean `Err`, never a panic.
///
/// Each export call resets the fuel budget and epoch deadline, then runs the
/// synchronous call on a blocking thread raced against the timeout: a trap or
/// an infinite loop inside an export is isolated to this call and can never
/// crash the daemon. On a successful call the instance is retained for further
/// calls (so a stateful export sequence works — Task 10's gateway
/// start/…/stop); after a timeout the trap-poisoned instance is dropped rather
/// than reused.
pub struct ComponentInstance {
    // `Option` so the instance + store can be moved into `spawn_blocking` and
    // handed back on the success path; `None` after a consumed/timed-out call.
    inner: Option<(Instance, Store<CapabilityState>)>,
    engine: Engine,
    fuel: u64,
    timeout: Duration,
}

impl ComponentInstance {
    /// Run `f` against this instance's exports under a fresh fuel budget and
    /// the epoch timeout. `f` receives the `Instance` (a lightweight `Copy`
    /// handle) and its `&mut Store`, runs on a blocking thread, and returns a
    /// `wasmtime::Result`; a trap (guest `unreachable`, host-func error) or an
    /// out-of-fuel/epoch interrupt is caught and mapped to a
    /// [`PluginRuntimeError`] — never a panic or a hung daemon. A timeout
    /// yields [`PluginRuntimeError::TimeoutExceeded`].
    pub(crate) async fn call<F, R>(&mut self, f: F) -> Result<R, PluginRuntimeError>
    where
        F: FnOnce(Instance, &mut Store<CapabilityState>) -> wasmtime::Result<R> + Send + 'static,
        R: Send + 'static,
    {
        let (instance, mut store) = self.inner.take().ok_or_else(|| {
            PluginRuntimeError::InstantiationFailed(
                "component instance already consumed by a prior timed-out call".to_string(),
            )
        })?;
        let fuel = self.fuel;
        let timeout = self.timeout;

        let join_handle = tokio::task::spawn_blocking(move || {
            // Reset the per-call budget so a prior call's spend never bleeds
            // into this one and each call is independently bounded.
            let prepared = store
                .set_fuel(fuel)
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()));
            store.set_epoch_deadline(1);
            let result = match prepared {
                Ok(()) => f(instance, &mut store).map_err(map_component_error),
                Err(error) => Err(error),
            };
            (instance, store, result)
        });

        tokio::pin!(join_handle);

        tokio::select! {
            joined = &mut join_handle => match joined {
                Ok((instance, store, result)) => {
                    // Retain the instance for further calls on the success (or
                    // clean guest-error) path.
                    self.inner = Some((instance, store));
                    result
                }
                Err(join_error) => Err(PluginRuntimeError::InstantiationFailed(format!(
                    "component export task panicked: {join_error}"
                ))),
            },
            _ = tokio::time::sleep(timeout) => {
                self.engine.increment_epoch();
                match join_handle.await {
                    // The call exceeded its host deadline; the instance is now
                    // trap-poisoned, so it is deliberately NOT restored.
                    Ok(_) => Err(PluginRuntimeError::TimeoutExceeded { timeout }),
                    Err(join_error) => Err(PluginRuntimeError::InstantiationFailed(format!(
                        "component export task panicked: {join_error}"
                    ))),
                }
            }
        }
    }
}

/// The `wasmtime::component::Store<T>` state type for a linked component
/// instantiation. Holds everything the four linked `Host` trait impls below
/// need: which plugin is calling ([`PluginCapabilityContext`]), whether
/// network was granted (surfaced through `ryuzi:host/host`'s
/// `capabilities()` call, and used together with `network_allowlist` to gate
/// `ryuzi:http/http` — see `http_iface::Host::request` below), the plugin's
/// own declared network allowlist (from its bundle manifest's
/// `permissions.network`, independent of whether `allow_network` ended up
/// true), and a `Handle` back to the async runtime the outer `instantiate`
/// call is running on.
///
/// # Async bridge
/// `wasmtime::component::bindgen!`'s generated `Host` traits are
/// synchronous (`&mut self` methods returning `Result` directly, no
/// `async_trait`/`Future`) even though `ScopedSettings`/`PluginStorage`'s
/// own methods are `async` (they go through `Store::with_conn`). Each trait
/// method below bridges the gap with `self.rt.block_on(...)`: `rt` is a
/// `tokio::runtime::Handle` captured with `Handle::current()` on the async
/// caller's thread *before* the `spawn_blocking` closure that builds this
/// state runs (see `instantiate` above) — inside `spawn_blocking` there is
/// no ambient Tokio reactor to construct a new runtime from, so the handle
/// must be captured ahead of time and moved in.
pub(crate) struct CapabilityState {
    ctx: Arc<PluginCapabilityContext>,
    allow_network: bool,
    /// This plugin's bundle-declared network allowlist entries (bare
    /// hostnames or `*.`-prefixed wildcards). Populated by `instantiate`
    /// from `bundle.manifest.permissions.network` regardless of whether
    /// `allow_network` is true — the `http_iface::Host::request` impl below
    /// only ever consults it when `allow_network` holds, since the `http`
    /// instance is not even linked otherwise.
    network_allowlist: Vec<String>,
    /// Whether this bundle may set its own `Authorization` header on
    /// `ryuzi:http/http` requests — VERIFIED first-party bundles only, mirrored
    /// from [`HostPolicy::allow_self_auth`] at instantiation. Threaded into the
    /// per-request [`AllowedHttpClient`] in `http_iface::Host::request`.
    allow_self_auth: bool,
    /// This component's per-call epoch timeout (`policy.limits.timeout`),
    /// mirrored at instantiation. It bounds the per-request [`AllowedHttpClient`]
    /// in `http_iface::Host::request` so a stalled allowlisted server can never
    /// hang a host function past the epoch deadline the guest-only epoch
    /// interruption cannot preempt — see `capabilities::http::DEFAULT_HTTP_TIMEOUT`.
    http_timeout: Duration,
    rt: tokio::runtime::Handle,
    /// Minimal, locked-down WASI p2 context. Any real (std-built) component
    /// imports the WASI baseline (`wasi:io`, `wasi:cli`, …) even when it never
    /// performs I/O, so the linker must satisfy those imports for the
    /// component to instantiate at all. This context grants NOTHING beyond
    /// clocks/random: no preopened directories, no environment, no inherited
    /// stdio, no sockets — so `wasi:filesystem`/`wasi:sockets` host functions
    /// exist but have nothing to reach. Real outbound network stays gated by
    /// the host-mediated `ryuzi:http`/`ryuzi:oauth` capabilities, never WASI.
    wasi_ctx: wasmtime_wasi::WasiCtx,
    wasi_table: wasmtime::component::ResourceTable,
    /// Per-instance registry of host-owned `ryuzi:websocket` connections. Its
    /// [`WsRegistry`] owns every open socket + reader task; dropping this
    /// `CapabilityState` (on supervisor stop/restart) drops the registry, which
    /// aborts each reader task and closes each socket — see
    /// `capabilities::websocket`.
    ws: WsRegistry,
}

impl wasmtime_wasi::WasiView for CapabilityState {
    fn ctx(&mut self) -> wasmtime_wasi::WasiCtxView<'_> {
        wasmtime_wasi::WasiCtxView {
            ctx: &mut self.wasi_ctx,
            table: &mut self.wasi_table,
        }
    }
}

impl host_iface::Host for CapabilityState {
    fn get_plugin_info(&mut self) -> Result<host_iface::PluginInfo, host_iface::HostError> {
        let info = HostInfo::new(&self.ctx, self.allow_network);
        let (id, version) = info.plugin_info();
        Ok(host_iface::PluginInfo { id, version })
    }

    fn capabilities(&mut self) -> Result<host_iface::HostCapabilities, host_iface::HostError> {
        let info = HostInfo::new(&self.ctx, self.allow_network);
        let (network, filesystem, secrets) = info.capabilities();
        Ok(host_iface::HostCapabilities {
            network,
            filesystem,
            secrets,
        })
    }

    fn log(
        &mut self,
        _message: String,
        fields: Vec<host_iface::LogField>,
    ) -> Result<bool, host_iface::HostError> {
        let redacted_fields = fields
            .into_iter()
            .map(|field| {
                (
                    "plugin.field",
                    crate::plugins::capabilities::redact_log_field(&field.name, &field.value),
                )
            })
            .collect();
        self.ctx
            .telemetry
            .count("plugin.capability.log", redacted_fields);
        Ok(true)
    }
}

impl settings_iface::Host for CapabilityState {
    fn get(
        &mut self,
        key: String,
    ) -> Result<settings_iface::Setting, settings_iface::SettingsError> {
        let ctx = self.ctx.clone();
        let result = self
            .rt
            .block_on(async move { ScopedSettings::new(&ctx).get(&key).await });
        match result {
            Ok((key, value, secret)) => Ok(settings_iface::Setting { key, value, secret }),
            Err(SettingsErr::NotFound) => Err(settings_iface::SettingsError::NotFound),
            Err(SettingsErr::Invalid(message)) => {
                Err(settings_iface::SettingsError::Invalid(message))
            }
            Err(SettingsErr::Unavailable) => Err(settings_iface::SettingsError::Unavailable),
        }
    }

    fn set(
        &mut self,
        value: settings_iface::Setting,
    ) -> Result<settings_iface::Setting, settings_iface::SettingsError> {
        let ctx = self.ctx.clone();
        let settings_iface::Setting { key, value, .. } = value;
        let result = self
            .rt
            .block_on(async move { ScopedSettings::new(&ctx).set(&key, &value).await });
        match result {
            Ok((key, secret)) => Ok(settings_iface::Setting {
                key,
                value: String::new(),
                secret,
            }),
            Err(SettingsErr::NotFound) => Err(settings_iface::SettingsError::NotFound),
            Err(SettingsErr::Invalid(message)) => {
                Err(settings_iface::SettingsError::Invalid(message))
            }
            Err(SettingsErr::Unavailable) => Err(settings_iface::SettingsError::Unavailable),
        }
    }

    fn remove(&mut self, key: String) -> Result<bool, settings_iface::SettingsError> {
        let ctx = self.ctx.clone();
        let result = self
            .rt
            .block_on(async move { ScopedSettings::new(&ctx).remove(&key).await });
        match result {
            Ok(existed) => Ok(existed),
            Err(SettingsErr::NotFound) => Err(settings_iface::SettingsError::NotFound),
            Err(SettingsErr::Invalid(message)) => {
                Err(settings_iface::SettingsError::Invalid(message))
            }
            Err(SettingsErr::Unavailable) => Err(settings_iface::SettingsError::Unavailable),
        }
    }
}

impl storage_iface::Host for CapabilityState {
    fn get(
        &mut self,
        key: String,
    ) -> Result<storage_iface::StoredValue, storage_iface::StorageError> {
        let ctx = self.ctx.clone();
        let key_for_response = key.clone();
        let result = self
            .rt
            .block_on(async move { PluginStorage::new(&ctx).get(&key).await });
        match result {
            Ok(value) => Ok(storage_iface::StoredValue {
                key: key_for_response,
                value,
            }),
            Err(StorageErr::NotFound) => Err(storage_iface::StorageError::NotFound),
            Err(StorageErr::Denied) => Err(storage_iface::StorageError::Denied),
            Err(StorageErr::Failed(message)) => Err(storage_iface::StorageError::Failed(message)),
        }
    }

    fn put(
        &mut self,
        value: storage_iface::StoredValue,
    ) -> Result<storage_iface::StoredValue, storage_iface::StorageError> {
        let ctx = self.ctx.clone();
        let storage_iface::StoredValue { key, value } = value;
        let key_for_response = key.clone();
        let result = self
            .rt
            .block_on(async move { PluginStorage::new(&ctx).put(&key, value).await });
        match result {
            Ok(()) => Ok(storage_iface::StoredValue {
                key: key_for_response,
                value: Vec::new(),
            }),
            Err(StorageErr::NotFound) => Err(storage_iface::StorageError::NotFound),
            Err(StorageErr::Denied) => Err(storage_iface::StorageError::Denied),
            Err(StorageErr::Failed(message)) => Err(storage_iface::StorageError::Failed(message)),
        }
    }

    fn delete(&mut self, key: String) -> Result<bool, storage_iface::StorageError> {
        let ctx = self.ctx.clone();
        let result = self
            .rt
            .block_on(async move { PluginStorage::new(&ctx).delete(&key).await });
        match result {
            Ok(existed) => Ok(existed),
            Err(StorageErr::NotFound) => Err(storage_iface::StorageError::NotFound),
            Err(StorageErr::Denied) => Err(storage_iface::StorageError::Denied),
            Err(StorageErr::Failed(message)) => Err(storage_iface::StorageError::Failed(message)),
        }
    }
}

impl http_iface::Host for CapabilityState {
    /// Builds an [`AllowedHttpClient`] scoped to this plugin's declared
    /// `network_allowlist` fresh for every call (the client is cheap —
    /// `reqwest::Client` internally pools connections and is `Clone`-cheap
    /// itself, so there's no meaningful cost to not caching it on
    /// `CapabilityState`), then bridges the async request through
    /// `self.rt.block_on(...)` like every other adapter here. Header
    /// stripping (`Authorization`/`Host`/`Content-Length`) and per-hop
    /// redirect allowlist checks happen inside `AllowedHttpClient::request`
    /// itself — see `capabilities::http`'s module doc.
    fn request(
        &mut self,
        request: http_iface::HttpRequest,
    ) -> Result<http_iface::HttpResponse, http_iface::HttpError> {
        let allowlist = self.network_allowlist.clone();
        let allow_self_auth = self.allow_self_auth;
        let http_timeout = self.http_timeout;
        let http_iface::HttpRequest {
            method,
            url,
            headers,
            body,
        } = request;
        let headers = headers
            .into_iter()
            .map(|header| (header.name, header.value))
            .collect();
        let client = AllowedHttpClient::with_self_auth(allowlist, allow_self_auth, http_timeout);
        let result = self
            .rt
            .block_on(async move { client.request(&method, &url, headers, body).await });
        match result {
            Ok(response) => Ok(http_iface::HttpResponse {
                status: response.status,
                headers: response
                    .headers
                    .into_iter()
                    .map(|(name, value)| http_iface::Header { name, value })
                    .collect(),
                body: response.body,
            }),
            Err(HttpErr::InvalidRequest(message)) => {
                Err(http_iface::HttpError::InvalidRequest(message))
            }
            Err(HttpErr::Rejected) => Err(http_iface::HttpError::Rejected),
            Err(HttpErr::Unavailable) => Err(http_iface::HttpError::Unavailable),
            Err(HttpErr::Failed(message)) => Err(http_iface::HttpError::Failed(message)),
        }
    }
}

impl oauth_iface::Host for CapabilityState {
    fn authorized_request(
        &mut self,
        profile_id: String,
        request: oauth_iface::OauthRequest,
    ) -> Result<oauth_iface::AuthorizedResponse, oauth_iface::OauthError> {
        let ctx = self.ctx.clone();
        let oauth_iface::OauthRequest {
            method,
            url,
            headers,
            body,
        } = request;
        let headers = headers
            .into_iter()
            .map(|header| (header.name, header.value))
            .collect();
        // Thread the component's per-call epoch budget into the outbound
        // request, exactly as the `ryuzi:provider-auth` host does below — a
        // blocked host call is never preempted by the epoch deadline, so the
        // HTTP client's own timeout is what actually catches a stalled upstream.
        let http_timeout = self.http_timeout;
        let result = self.rt.block_on(async move {
            ProfileOauth::with_timeout(&ctx, http_timeout)
                .authorized_request(&profile_id, &method, &url, headers, body)
                .await
        });
        match result {
            Ok(response) => Ok(oauth_iface::AuthorizedResponse {
                status: response.status,
                headers: response
                    .headers
                    .into_iter()
                    .map(|(name, value)| oauth_iface::Header { name, value })
                    .collect(),
                body: response.body,
            }),
            Err(OauthErr::InvalidRequest(message)) => {
                Err(oauth_iface::OauthError::InvalidRequest(message))
            }
            Err(OauthErr::Denied) => Err(oauth_iface::OauthError::Denied),
            Err(OauthErr::Expired) => Err(oauth_iface::OauthError::Expired),
            Err(OauthErr::Failed(message)) => Err(oauth_iface::OauthError::Failed(message)),
        }
    }

    fn disconnect(&mut self, profile_id: String) -> Result<bool, oauth_iface::OauthError> {
        let ctx = self.ctx.clone();
        let result = self.rt.block_on(async move {
            ProfileOauth::new(&ctx)
                .disconnect_profile(&profile_id)
                .await
        });
        match result {
            Ok(()) => Ok(true),
            Err(OauthErr::InvalidRequest(message)) => {
                Err(oauth_iface::OauthError::InvalidRequest(message))
            }
            Err(OauthErr::Denied) => Err(oauth_iface::OauthError::Denied),
            Err(OauthErr::Expired) => Err(oauth_iface::OauthError::Expired),
            Err(OauthErr::Failed(message)) => Err(oauth_iface::OauthError::Failed(message)),
        }
    }
}

/// Thin `ryuzi:provider-auth` adapter (Task 16c1): maps the generated WIT types
/// to the adapter-local ones and bridges the async call through
/// `self.rt.block_on(...)` exactly like `oauth_iface::Host` above. Caller
/// authorization, credential lookup, descriptor-driven injection, and the
/// allowlist/redirect enforcement all live in `capabilities::provider_auth`;
/// this is only the binding layer. Note the guest never gets a "which
/// credential is stored" oracle beyond the typed `not-configured` error, and no
/// variant carries credential material.
impl provider_auth_iface::Host for CapabilityState {
    fn authorized_request(
        &mut self,
        provider_id: String,
        request: provider_auth_iface::ProviderRequest,
    ) -> Result<provider_auth_iface::ProviderResponse, provider_auth_iface::ProviderAuthError> {
        let ctx = self.ctx.clone();
        let provider_auth_iface::ProviderRequest {
            method,
            url,
            headers,
            body,
        } = request;
        let headers = headers
            .into_iter()
            .map(|header| (header.name, header.value))
            .collect();
        let http_timeout = self.http_timeout;
        let result = self.rt.block_on(async move {
            ProviderAuth::with_timeout(&ctx, http_timeout)
                .authorized_request(&provider_id, &method, &url, headers, body)
                .await
        });
        match result {
            Ok(response) => Ok(provider_auth_iface::ProviderResponse {
                status: response.status,
                headers: response
                    .headers
                    .into_iter()
                    .map(|(name, value)| provider_auth_iface::Header { name, value })
                    .collect(),
                body: response.body,
            }),
            Err(ProviderAuthErr::InvalidRequest(message)) => Err(
                provider_auth_iface::ProviderAuthError::InvalidRequest(message),
            ),
            Err(ProviderAuthErr::Denied) => Err(provider_auth_iface::ProviderAuthError::Denied),
            Err(ProviderAuthErr::NotConfigured) => {
                Err(provider_auth_iface::ProviderAuthError::NotConfigured)
            }
            Err(ProviderAuthErr::Rejected) => Err(provider_auth_iface::ProviderAuthError::Rejected),
            Err(ProviderAuthErr::Unavailable) => {
                Err(provider_auth_iface::ProviderAuthError::Unavailable)
            }
            Err(ProviderAuthErr::Failed(message)) => {
                Err(provider_auth_iface::ProviderAuthError::Failed(message))
            }
        }
    }
}

/// Thin `ryuzi:websocket` adapter: maps the generated WIT types to the
/// registry's adapter-local types and drives `self.ws` (the per-instance
/// [`WsRegistry`]) using this state's captured network allowlist and runtime
/// handle. All the socket ownership, allowlist/scheme gating, per-instance
/// caps, and lifecycle live in `capabilities::websocket`; this is only the
/// binding layer (mirroring `http_iface::Host` above).
impl websocket_iface::Host for CapabilityState {
    fn connect(
        &mut self,
        url: String,
        headers: Vec<websocket_iface::WsHeader>,
    ) -> Result<u64, websocket_iface::WsError> {
        let headers = headers
            .into_iter()
            .map(|header| WsHeader {
                name: header.name,
                value: header.value,
            })
            .collect();
        self.ws
            .connect(
                &self.network_allowlist,
                &self.rt,
                self.http_timeout,
                &url,
                headers,
            )
            .map_err(map_ws_err)
    }

    fn send(
        &mut self,
        handle: u64,
        frame: websocket_iface::WsFrame,
    ) -> Result<(), websocket_iface::WsError> {
        let frame = WsFrame {
            data: frame.data,
            is_text: frame.is_text,
        };
        self.ws
            .send(&self.rt, self.http_timeout, handle, frame)
            .map_err(map_ws_err)
    }

    fn poll(
        &mut self,
        handle: u64,
    ) -> Result<Vec<websocket_iface::WsFrame>, websocket_iface::WsError> {
        self.ws
            .poll(handle)
            .map(|frames| {
                frames
                    .into_iter()
                    .map(|frame| websocket_iface::WsFrame {
                        data: frame.data,
                        is_text: frame.is_text,
                    })
                    .collect()
            })
            .map_err(map_ws_err)
    }

    fn state(&mut self, handle: u64) -> Result<websocket_iface::WsState, websocket_iface::WsError> {
        self.ws
            .state(handle)
            .map(|state| match state {
                WsConnState::Connecting => websocket_iface::WsState::Connecting,
                WsConnState::Open => websocket_iface::WsState::Open,
                WsConnState::Closing => websocket_iface::WsState::Closing,
                WsConnState::Closed => websocket_iface::WsState::Closed,
            })
            .map_err(map_ws_err)
    }

    fn close(&mut self, handle: u64) -> Result<(), websocket_iface::WsError> {
        self.ws.close(handle).map_err(map_ws_err)
    }
}

/// Maps the `capabilities::websocket` adapter-local error to the generated WIT
/// `websocket::WsError`.
fn map_ws_err(error: WsErr) -> websocket_iface::WsError {
    match error {
        WsErr::InvalidRequest(message) => websocket_iface::WsError::InvalidRequest(message),
        WsErr::Rejected => websocket_iface::WsError::Rejected,
        WsErr::Disconnected => websocket_iface::WsError::Disconnected,
        WsErr::LimitExceeded(message) => websocket_iface::WsError::LimitExceeded(message),
        WsErr::Failed(message) => websocket_iface::WsError::Failed(message),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::SettingsStore;
    use crate::store::ComponentPluginReleaseRecord;
    use crate::telemetry::NoopTelemetry;
    use ryuzi_plugin_sdk::{NetworkPermission, PluginLifecycle, PluginPermissions, PluginRelease};

    /// A throwaway [`PluginCapabilityContext`] over a fresh on-disk `Store` —
    /// enough for `instantiate` tests that don't exercise the settings/
    /// storage adapters themselves (those are covered directly in
    /// `capabilities::settings`/`capabilities::storage`). Returns the
    /// backing tempfile too, so it isn't dropped (and the DB deleted) before
    /// the test finishes using the context.
    async fn test_ctx(plugin_id: &str) -> (Arc<PluginCapabilityContext>, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        (
            Arc::new(PluginCapabilityContext {
                plugin_id: plugin_id.to_string(),
                version: "0.1.0".to_string(),
                settings: SettingsStore::new(store.clone()),
                store,
                telemetry: Arc::new(NoopTelemetry),
                network_allowlist: vec![],
                oauth_profile_ids: vec![],
                provider_ids: vec![],
            }),
            tmp,
        )
    }

    fn manifest(network: Vec<&str>) -> PluginBundleManifest {
        PluginBundleManifest {
            id: "acme".to_string(),
            name: "Acme".to_string(),
            version: "0.1.0".to_string(),
            wit_api: "^0.1.0".to_string(),
            lifecycle: PluginLifecycle::Singleton,
            component: "plugin.wasm".to_string(),
            publisher: String::new(),
            description: String::new(),
            permissions: PluginPermissions {
                network: network
                    .into_iter()
                    .map(|host| NetworkPermission(host.to_string()))
                    .collect(),
            },
            oauth: vec![],
            provider_ids: vec![],
        }
    }

    /// A `PluginRelease` whose fields are usable directly by tests that only
    /// exercise [`ComponentRuntime::validate_component`] (which reads the
    /// manifest, not the release, off a [`VerifiedBundle`]).
    fn release() -> PluginRelease {
        PluginRelease {
            id: "acme".to_string(),
            version: "0.1.0".to_string(),
            wit_api: "0.1.0".to_string(),
            component_url: "https://example.invalid/acme/plugin.wasm".to_string(),
            component_sha256: "0".repeat(64),
            size_bytes: None,
            published_at: None,
        }
    }

    fn fixture_artifact(name: &str) -> std::path::PathBuf {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        root.join("tests")
            .join("fixtures")
            .join(name)
            .join("target")
            .join("wasm32-wasip2")
            .join("release")
            .join(match name {
                "component-noop" => "ryuzi_component_noop_fixture.wasm",
                "component-http-import" => "ryuzi_component_http_fixture.wasm",
                "component-connector" => "ryuzi_component_connector_fixture.wasm",
                "component-hooks" => "ryuzi_component_hooks_fixture.wasm",
                "component-hooks-loop" => "ryuzi_component_hooks_loop_fixture.wasm",
                "component-provider" => "ryuzi_component_provider_fixture.wasm",
                "component-gateway" => "ryuzi_component_gateway_fixture.wasm",
                "component-websocket-import" => "ryuzi_component_websocket_fixture.wasm",
                "component-provider-auth-import" => "ryuzi_component_provider_auth_fixture.wasm",
                _ => panic!("unknown fixture {name}"),
            })
    }

    fn build_fixture_components() {
        // Shared process-once build so concurrent fixture tests (here and in
        // `wasm_connector`/`wasm_hooks`) never race `build-components.sh`'s
        // non-atomic `wit/deps/` rewrite.
        crate::plugins::build_fixture_components_once();
    }

    #[test]
    fn real_fixture_components_expose_expected_wit_contracts() {
        build_fixture_components();
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let noop = Component::from_file(&runtime.engine, fixture_artifact("component-noop"))
            .expect("noop fixture component should parse");
        let noop_imports: Vec<_> = noop
            .component_type()
            .imports(&runtime.engine)
            .map(|(name, _)| name.to_string())
            .collect();
        let noop_exports: Vec<_> = noop
            .component_type()
            .exports(&runtime.engine)
            .map(|(name, _)| name.to_string())
            .collect();
        assert!(noop_exports.iter().any(|name| name == LIFECYCLE_EXPORT));
        assert!(!noop_imports.iter().any(|name| name == HTTP_IMPORT));
        runtime
            .validate_component_bytes(
                &manifest(vec![]),
                &std::fs::read(fixture_artifact("component-noop")).unwrap(),
                &HostPolicy::deny_all(),
            )
            .expect("noop fixture should validate with WASI and types baseline imports");

        let http = Component::from_file(&runtime.engine, fixture_artifact("component-http-import"))
            .expect("HTTP fixture component should parse");
        let http_imports: Vec<_> = http
            .component_type()
            .imports(&runtime.engine)
            .map(|(name, _)| name.to_string())
            .collect();
        assert!(http_imports.iter().any(|name| name == HTTP_IMPORT));
        let result = runtime.validate_component_bytes(
            &manifest(vec![]),
            &std::fs::read(fixture_artifact("component-http-import")).unwrap(),
            &HostPolicy::deny_all(),
        );
        assert!(
            matches!(result, Err(PluginRuntimeError::DeniedImport { name, .. }) if name == HTTP_IMPORT)
        );
    }
    fn component_release() -> ComponentPluginReleaseRecord {
        ComponentPluginReleaseRecord {
            plugin_id: "acme".to_string(),
            version: "0.1.0".to_string(),
            source_url: "https://example.invalid/acme/plugin.wasm".to_string(),
            sha256: "0".repeat(64),
            signing_key_id: "test".to_string(),
            installed_at: 0,
            active: true,
            revoked: false,
            revocation_reason: None,
        }
    }

    fn installed_bundle(dir: &std::path::Path) -> InstalledBundle {
        let component_path = dir.join("plugin.wasm");
        std::fs::write(&component_path, b"(component)").expect("writing component should succeed");
        InstalledBundle {
            manifest: manifest(vec![]),
            release: release(),
            release_record: component_release(),
            root: dir.to_path_buf(),
            component_path,
        }
    }

    /// Creates an [`InstalledBundle`] whose component bytes are the given WAT
    /// string (compiled to a component by the engine).  The manifest uses the
    /// default test id/name so export-validation passes for import-free
    /// components.
    fn installed_bundle_with_wat(dir: &std::path::Path, wat: &str) -> InstalledBundle {
        let component_path = dir.join("plugin.wasm");
        std::fs::write(&component_path, wat.as_bytes())
            .expect("writing WAT component should succeed");
        InstalledBundle {
            manifest: manifest(vec![]),
            release: release(),
            release_record: component_release(),
            root: dir.to_path_buf(),
            component_path,
        }
    }

    #[test]
    fn host_policy_for_installed_bundle_gates_self_auth_on_the_first_party_key() {
        use crate::plugins::first_party_key::FIRST_PARTY_KEY_ID;

        fn bundle_with(signing_key_id: &str, network: Vec<&str>) -> InstalledBundle {
            let mut record = component_release();
            record.signing_key_id = signing_key_id.to_string();
            InstalledBundle {
                manifest: manifest(network),
                release: release(),
                release_record: record,
                root: std::path::PathBuf::from("nonexistent"),
                component_path: std::path::PathBuf::from("nonexistent/plugin.wasm"),
            }
        }

        // The security-sensitive gate is bound to VERIFIED install provenance:
        // only a bundle whose recorded signing key id is the first-party key
        // (set by `verify_bundle` from the trusted-key match) gets self-auth.
        let first_party =
            HostPolicy::for_installed_bundle(&bundle_with(FIRST_PARTY_KEY_ID, vec![]));
        assert!(
            first_party.allow_self_auth,
            "the first-party key must grant self-auth"
        );

        let third_party =
            HostPolicy::for_installed_bundle(&bundle_with("some-other-registry", vec![]));
        assert!(
            !third_party.allow_self_auth,
            "a non-first-party key must NOT grant self-auth"
        );

        // The rest of the derivation is manifest-driven and independent of the
        // self-auth gate: settings/storage always on, network only with a
        // declared host — and a network grant never implies self-auth.
        assert!(first_party.allow_settings && first_party.allow_storage);
        assert!(
            !first_party.allow_network,
            "no manifest hosts => no network"
        );
        let networked = HostPolicy::for_installed_bundle(&bundle_with(
            "some-other-registry",
            vec!["api.example.com"],
        ));
        assert!(networked.allow_network, "a manifest host grants network");
        assert!(
            !networked.allow_self_auth,
            "a network grant must not imply self-auth"
        );
    }

    #[tokio::test]
    async fn instantiate_succeeds_for_an_installed_component_under_deny_all() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let (ctx, _tmp) = test_ctx("acme").await;
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        runtime
            .instantiate(&installed_bundle(dir.path()), HostPolicy::deny_all(), ctx)
            .await
            .expect("an import-free installed component should instantiate");
    }

    /// Positive instantiation proof for a host CAPABILITY import. The
    /// `component-http-import` fixture imports `ryuzi:http/http@0.1.0`, so once
    /// network is granted it must instantiate — which only works if the http
    /// adapter is linked under the interface's FULLY-QUALIFIED import name.
    ///
    /// Regression guard: every capability adapter used to be linked under a
    /// SHORT instance name (`linker.instance("http"/"oauth"/…)`) that no
    /// component import — which is keyed by the fully-qualified id
    /// (`ryuzi:http/http@0.1.0`) — can match. Every fixture except this one
    /// imports only `ryuzi:plugin/types`, so nothing ever instantiated a
    /// capability import positively and the mismatch stayed latent until the
    /// first first-party component to import `ryuzi:oauth` (the github
    /// connector) hit it. This test fails before that fix and passes after.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn instantiate_links_a_capability_import_by_its_full_interface_name() {
        build_fixture_components();
        let component_path = fixture_artifact("component-http-import");
        let root = component_path
            .parent()
            .expect("fixture artifact has a parent dir")
            .to_path_buf();
        let bundle = InstalledBundle {
            manifest: manifest(vec!["fixture.invalid"]),
            release: release(),
            release_record: component_release(),
            root,
            component_path,
        };
        // A declared network host grants the http capability under host policy.
        let policy = HostPolicy::for_installed_bundle(&bundle);
        assert!(policy.allow_network, "a manifest host must grant network");
        let (ctx, _tmp) = test_ctx("acme").await;
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        runtime
            .instantiate(&bundle, policy, ctx)
            .await
            .expect("a component importing ryuzi:http/http must instantiate once http is linked");
    }

    /// An [`InstalledBundle`] for the given prebuilt fixture artifact with an
    /// explicit network allowlist (the caller must `build_fixture_components()`
    /// first). Unlike [`installed_fixture_bundle`], the manifest's network
    /// permissions are caller-supplied so `HostPolicy::for_installed_bundle`
    /// can derive `allow_network`/`allow_websocket` from them.
    fn installed_fixture_bundle_with_network(name: &str, network: Vec<&str>) -> InstalledBundle {
        let component_path = fixture_artifact(name);
        let root = component_path
            .parent()
            .expect("fixture artifact has a parent dir")
            .to_path_buf();
        InstalledBundle {
            manifest: manifest(network),
            release: release(),
            release_record: component_release(),
            root,
            component_path,
        }
    }

    /// Positive instantiation proof for the `ryuzi:websocket` capability: the
    /// `component-websocket-import` fixture imports
    /// `ryuzi:websocket/websocket@0.1.0`, so once the manifest declares a
    /// network host — which grants `allow_websocket` — it must instantiate.
    /// That only works if the websocket adapter is linked under the interface's
    /// FULLY-QUALIFIED import name (the Task-13b regression guard, mirrored
    /// here for websocket).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn instantiate_links_the_websocket_capability_by_full_interface_id() {
        build_fixture_components();
        let bundle = installed_fixture_bundle_with_network(
            "component-websocket-import",
            vec!["gateway.example"],
        );
        let policy = HostPolicy::for_installed_bundle(&bundle);
        assert!(
            policy.allow_websocket,
            "a declared network host must grant the websocket capability"
        );
        let (ctx, _tmp) = test_ctx("wsimport").await;
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        runtime.instantiate(&bundle, policy, ctx).await.expect(
            "a component importing ryuzi:websocket must instantiate once the adapter is linked",
        );
    }

    /// Deny proof: with an EMPTY network allowlist `allow_websocket` is false,
    /// so a component importing `ryuzi:websocket` is rejected before it can
    /// instantiate — and the error names the denied interface.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn websocket_import_without_network_is_denied() {
        build_fixture_components();
        let bundle = installed_fixture_bundle_with_network("component-websocket-import", vec![]);
        let policy = HostPolicy::for_installed_bundle(&bundle);
        assert!(
            !policy.allow_websocket,
            "no manifest network host must leave the websocket capability denied"
        );
        let (ctx, _tmp) = test_ctx("wsimport").await;
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let error = runtime
            .instantiate(&bundle, policy, ctx)
            .await
            .expect_err("a websocket import with no network grant must be rejected");
        match error {
            PluginRuntimeError::DeniedImport { name, .. }
            | PluginRuntimeError::InstantiationFailed(name)
                if name.contains("ryuzi:websocket") => {}
            other => panic!("expected a ryuzi:websocket denial, got {other:?}"),
        }
    }

    /// An [`InstalledBundle`] pointing at a real prebuilt fixture artifact
    /// (the caller must `build_fixture_components()` first).
    fn installed_fixture_bundle(name: &str) -> InstalledBundle {
        let component_path = fixture_artifact(name);
        let root = component_path
            .parent()
            .expect("fixture artifact has a parent dir")
            .to_path_buf();
        InstalledBundle {
            manifest: manifest(vec![]),
            release: release(),
            release_record: component_release(),
            root,
            component_path,
        }
    }

    /// End-to-end foundation proof: `compile` + `instantiate` produce a live
    /// [`ComponentInstance`], and the generic [`ComponentInstance::call`]
    /// reaches a real component's export through the generated per-interface
    /// `Guest::new` accessor — the same seam the connector/hooks adapters and
    /// (Task 10) provider/gateway adapters use. The noop fixture exports only
    /// `ryuzi:plugin/lifecycle`, so this also proves a SUBSET-exporting
    /// component is callable without the full `ryuzi:plugin` world.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn call_reaches_a_subset_components_lifecycle_export() {
        build_fixture_components();
        let (ctx, _tmp) = test_ctx("acme").await;
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let compiled = runtime
            .compile(
                &installed_fixture_bundle("component-noop"),
                HostPolicy::deny_all(),
            )
            .expect("noop fixture should compile under deny-all");
        let mut instance = compiled
            .instantiate(ctx)
            .await
            .expect("noop fixture should instantiate");
        let health = instance
            .call(|inst, store| {
                use crate::plugins::capabilities::wit_bindings::exports::ryuzi::plugin::lifecycle;
                let pre = inst.instance_pre(&*store);
                let guest = lifecycle::GuestIndices::new(&pre)?.load(&mut *store, &inst)?;
                guest.call_health(&mut *store)
            })
            .await
            .expect("call must not surface a runtime error")
            .expect("lifecycle.health must return Ok");
        assert!(health.healthy, "noop fixture reports healthy");
    }

    /// A component-level WAT whose core module start function is a
    /// nonterminating loop.  Combined with `fuel: u64::MAX` and a short
    /// timeout, this exercises the timeout enforcement path of the public
    /// [`ComponentRuntime::instantiate`].
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn instantiate_times_out_on_nonterminating_component() {
        // WAT component: core module with an infinite-loop start function.
        // No component-level imports or exports, so manifest validation passes.
        // The start function must have type () -> () (no result).
        let loop_wat = "(component \
            (core module $m \
                (func $loop \
                    (loop \
                        (br 0) \
                    ) \
                ) \
                (start $loop) \
            ) \
            (core instance (instantiate $m)) \
        )";

        let dir = tempfile::tempdir().expect("tempdir should create");
        let bundle = installed_bundle_with_wat(dir.path(), loop_wat);
        let (ctx, _tmp) = test_ctx("acme").await;

        let mut policy = HostPolicy::deny_all();
        policy.limits.fuel = u64::MAX;
        policy.limits.timeout = Duration::from_millis(100);

        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let error = runtime
            .instantiate(&bundle, policy, ctx)
            .await
            .expect_err("nonterminating component must not succeed");
        assert!(
            matches!(error, PluginRuntimeError::TimeoutExceeded { .. }),
            "expected TimeoutExceeded, got {error:?}"
        );
    }

    fn component_with_export(name: &str) -> String {
        format!(
            r#"(component
                (component $inner
                    (type $t string)
                    (export "t" (type $t))
                )
                (instance $i (instantiate $inner))
                (export "{name}" (instance $i))
            )"#
        )
    }

    #[test]
    fn default_resource_limits_are_conservative() {
        assert_eq!(
            ResourceLimits::default(),
            ResourceLimits {
                max_memory_bytes: 64 * 1024 * 1024,
                fuel: 10_000_000,
                timeout: Duration::from_secs(30),
                max_concurrency: 4,
            }
        );
    }

    #[test]
    fn unknown_component_export_is_denied() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let result = runtime.validate_component_bytes(
            &manifest(vec![]),
            component_with_export("acme:evil/thing@0.1.0").as_bytes(),
            &HostPolicy::deny_all(),
        );
        let Err(error) = result else {
            panic!("undeclared export must not validate");
        };
        assert!(
            matches!(error, PluginRuntimeError::DeniedExport { name, .. } if name == "acme:evil/thing@0.1.0")
        );
    }

    #[test]
    fn lifecycle_component_export_is_allowed() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        runtime
            .validate_component_bytes(
                &manifest(vec![]),
                component_with_export("lifecycle").as_bytes(),
                &HostPolicy::deny_all(),
            )
            .expect("lifecycle export is part of the plugin world");
    }

    #[test]
    fn core_module_execution_reports_fuel_exhaustion() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let error = runtime
            .execute_core_module_with_fuel("(module (func (loop br 0)) (start 0))", 100)
            .expect_err("infinite loop must exhaust fuel");
        assert!(matches!(error, PluginRuntimeError::FuelExhausted(_)));
    }

    #[test]
    fn empty_component_validates_under_deny_all() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        runtime
            .validate_component_bytes(&manifest(vec![]), b"(component)", &HostPolicy::deny_all())
            .expect("component without imports should validate");
    }

    #[test]
    fn malformed_component_returns_a_clean_error() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let result = runtime.validate_component_bytes(
            &manifest(vec![]),
            b"not wasm",
            &HostPolicy::deny_all(),
        );
        let Err(error) = result else {
            panic!("invalid bytes must not validate");
        };
        assert!(matches!(error, PluginRuntimeError::MalformedComponent(_)));
    }

    #[test]
    fn http_import_without_network_permission_is_denied() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let result = runtime.validate_component_bytes(
            &manifest(vec![]),
            br#"(component (import "ryuzi:http/http@0.1.0" (instance)))"#,
            &HostPolicy::deny_all(),
        );
        let Err(error) = result else {
            panic!("HTTP must require manifest and host approval");
        };
        assert!(
            matches!(error, PluginRuntimeError::DeniedImport { name, .. } if name == "ryuzi:http/http@0.1.0")
        );
    }

    #[test]
    fn http_import_is_allowed_with_manifest_allowlist_and_network_grant() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let policy = HostPolicy {
            allow_network: true,
            ..HostPolicy::deny_all()
        };
        runtime
            .validate_component_bytes(
                &manifest(vec!["api.github.com"]),
                br#"(component (import "ryuzi:http/http@0.1.0" (instance)))"#,
                &policy,
            )
            .expect("http import must validate with a manifest allowlist and a network grant");
    }

    #[test]
    fn oauth_import_without_policy_grant_is_denied() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let result = runtime.validate_component_bytes(
            &manifest(vec![]),
            br#"(component (import "ryuzi:oauth/oauth@0.2.0" (instance)))"#,
            &HostPolicy::deny_all(),
        );
        let Err(error) = result else {
            panic!("OAuth must require host policy approval");
        };
        assert!(
            matches!(error, PluginRuntimeError::DeniedImport { name, .. } if name == OAUTH_IMPORT)
        );
    }

    #[test]
    fn oauth_import_is_allowed_when_policy_grants_it() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let policy = HostPolicy {
            allow_oauth: true,
            ..HostPolicy::deny_all()
        };
        runtime
            .validate_component_bytes(
                &manifest(vec![]),
                br#"(component (import "ryuzi:oauth/oauth@0.2.0" (instance)))"#,
                &policy,
            )
            .expect("OAuth import must validate once host policy grants it");
    }

    /// Positive INSTANTIATION proof for `ryuzi:provider-auth` — the guard for
    /// the Task-13b regression class: an adapter linked under a short instance
    /// name can never satisfy a component's import, which is always keyed by
    /// the fully-qualified interface id. This drives the REAL
    /// `component-provider-auth-import` fixture (which actually calls the
    /// import, so wit-bindgen retains the import edge); a hand-written WAT
    /// component with an empty instance type would NOT catch the regression,
    /// because wasmtime satisfies an empty instance import trivially.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn instantiate_links_the_provider_auth_capability_by_full_interface_id() {
        const FIXTURE: &str = "component-provider-auth-import";
        build_fixture_components();
        let mut bundle = installed_fixture_bundle_with_network(FIXTURE, vec!["api.openai.com"]);
        bundle.manifest.provider_ids = vec!["openai".to_string()];
        let policy = HostPolicy::for_installed_bundle(&bundle);
        assert!(
            policy.allow_provider_auth,
            "declared provider-ids + a network host must grant provider-auth"
        );
        let (ctx, _tmp) = test_ctx("acme").await;
        let runtime = ComponentRuntime::new().expect("runtime should configure");

        // The fixture must actually IMPORT the interface, or the instantiation
        // below would prove nothing about linking (mirrors the HTTP_IMPORT
        // assertion in `real_fixture_components_expose_expected_wit_contracts`).
        let component = Component::from_file(&runtime.engine, fixture_artifact(FIXTURE))
            .expect("provider-auth fixture component should parse");
        let imports: Vec<_> = component
            .component_type()
            .imports(&runtime.engine)
            .map(|(name, _)| name.to_string())
            .collect();
        assert!(
            imports.iter().any(|name| name == PROVIDER_AUTH_IMPORT),
            "the fixture must import {PROVIDER_AUTH_IMPORT}; got {imports:?}"
        );

        runtime.instantiate(&bundle, policy, ctx).await.expect(
            "a component importing ryuzi:provider-auth must instantiate once the adapter is linked",
        );
    }

    /// Deny proof on the same real fixture: without declared `provider-ids` the
    /// policy withholds the grant, so the component is rejected before it can
    /// instantiate — and the error names the denied interface.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_auth_import_without_declared_provider_ids_is_denied() {
        build_fixture_components();
        let bundle = installed_fixture_bundle_with_network(
            "component-provider-auth-import",
            vec!["api.openai.com"],
        );
        let policy = HostPolicy::for_installed_bundle(&bundle);
        assert!(
            !policy.allow_provider_auth,
            "no declared provider-ids must leave provider-auth denied"
        );
        let (ctx, _tmp) = test_ctx("acme").await;
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let error = runtime
            .instantiate(&bundle, policy, ctx)
            .await
            .expect_err("a provider-auth import with no grant must be rejected");
        match error {
            PluginRuntimeError::DeniedImport { name, .. }
            | PluginRuntimeError::InstantiationFailed(name)
                if name.contains("ryuzi:provider-auth") => {}
            other => panic!("expected a ryuzi:provider-auth denial, got {other:?}"),
        }
    }

    #[test]
    fn provider_auth_import_without_policy_grant_is_denied() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let result = runtime.validate_component_bytes(
            &manifest(vec!["api.openai.com"]),
            br#"(component (import "ryuzi:provider-auth/provider-auth@0.1.0" (instance)))"#,
            &HostPolicy::deny_all(),
        );
        let Err(error) = result else {
            panic!("provider credential injection must require host policy approval");
        };
        assert!(
            matches!(error, PluginRuntimeError::DeniedImport { name, .. } if name == PROVIDER_AUTH_IMPORT)
        );
    }

    #[test]
    fn provider_auth_import_is_allowed_when_policy_grants_it() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let policy = HostPolicy {
            allow_provider_auth: true,
            ..HostPolicy::deny_all()
        };
        runtime
            .validate_component_bytes(
                &manifest(vec!["api.openai.com"]),
                br#"(component (import "ryuzi:provider-auth/provider-auth@0.1.0" (instance)))"#,
                &policy,
            )
            .expect("provider-auth import must validate once host policy grants it");
    }

    /// The provider-auth grant is fail-closed and needs BOTH halves of the
    /// declaration: explicit manifest `provider-ids` (the `[id]` fallback of
    /// `resolved_provider_ids` must not silently hand a bundle a credential)
    /// and a network allowlist.
    #[test]
    fn host_policy_grants_provider_auth_only_with_declared_provider_ids_and_network() {
        fn bundle_with(provider_ids: Vec<&str>, network: Vec<&str>) -> InstalledBundle {
            let mut manifest = manifest(network);
            manifest.provider_ids = provider_ids.into_iter().map(str::to_string).collect();
            InstalledBundle {
                manifest,
                release: release(),
                release_record: component_release(),
                root: std::path::PathBuf::from("nonexistent"),
                component_path: std::path::PathBuf::from("nonexistent/plugin.wasm"),
            }
        }

        assert!(
            HostPolicy::for_installed_bundle(&bundle_with(vec!["openai"], vec!["api.openai.com"]))
                .allow_provider_auth,
            "declared provider-ids + a network host must grant provider-auth"
        );
        assert!(
            !HostPolicy::for_installed_bundle(&bundle_with(vec![], vec!["api.openai.com"]))
                .allow_provider_auth,
            "the resolved-provider-id fallback must NOT grant provider-auth"
        );
        assert!(
            !HostPolicy::for_installed_bundle(&bundle_with(vec!["openai"], vec![]))
                .allow_provider_auth,
            "no declared network host must leave provider-auth denied"
        );
        assert!(
            !HostPolicy::deny_all().allow_provider_auth,
            "deny-all must deny provider-auth"
        );
    }

    /// A bundle that DOES declare `provider-ids` carries them into the
    /// capability context the guest's calls are authorized against, so the
    /// authorization set is manifest-derived and never caller-supplied.
    #[tokio::test]
    async fn compile_carries_declared_provider_ids_into_the_capability_context() {
        build_fixture_components();
        let mut bundle = installed_fixture_bundle("component-noop");
        bundle.manifest.provider_ids = vec!["mimo-free".to_string()];
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let compiled = runtime
            .compile(&bundle, HostPolicy::for_installed_bundle(&bundle))
            .expect("noop fixture should compile");
        assert_eq!(compiled.provider_ids, vec!["mimo-free".to_string()]);

        // ONE rule governs the credential grant: the EXPLICIT manifest
        // `provider-ids`. A bundle that declares none gets none — the
        // `resolved_provider_ids` `[id]` fallback (for transport registration)
        // must never seed a credential authorization set.
        let undeclared = installed_fixture_bundle("component-noop");
        assert!(undeclared.manifest.provider_ids.is_empty());
        assert_eq!(
            undeclared.manifest.resolved_provider_ids(),
            vec![undeclared.manifest.id.clone()],
            "the transport fallback still resolves to the bundle id"
        );
        let compiled = runtime
            .compile(&undeclared, HostPolicy::for_installed_bundle(&undeclared))
            .expect("noop fixture should compile");
        assert!(
            compiled.provider_ids.is_empty(),
            "an undeclared bundle must carry no provider credential authorization"
        );
    }

    #[test]
    fn settings_import_without_policy_grant_is_denied() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let result = runtime.validate_component_bytes(
            &manifest(vec![]),
            br#"(component (import "ryuzi:settings/settings@0.1.0" (instance)))"#,
            &HostPolicy::deny_all(),
        );
        let Err(error) = result else {
            panic!("settings import must require a policy grant");
        };
        assert!(
            matches!(error, PluginRuntimeError::DeniedImport { name, .. } if name == SETTINGS_IMPORT)
        );
    }

    #[test]
    fn settings_import_is_allowed_when_policy_grants_it() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let policy = HostPolicy {
            allow_settings: true,
            ..HostPolicy::deny_all()
        };
        runtime
            .validate_component_bytes(
                &manifest(vec![]),
                br#"(component (import "ryuzi:settings/settings@0.1.0" (instance)))"#,
                &policy,
            )
            .expect("settings import must validate once the policy grants it");
    }

    #[test]
    fn storage_import_without_policy_grant_is_denied() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let result = runtime.validate_component_bytes(
            &manifest(vec![]),
            br#"(component (import "ryuzi:storage/storage@0.1.0" (instance)))"#,
            &HostPolicy::deny_all(),
        );
        let Err(error) = result else {
            panic!("storage import must require a policy grant");
        };
        assert!(
            matches!(error, PluginRuntimeError::DeniedImport { name, .. } if name == STORAGE_IMPORT)
        );
    }

    #[test]
    fn storage_import_is_allowed_when_policy_grants_it() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let policy = HostPolicy {
            allow_storage: true,
            ..HostPolicy::deny_all()
        };
        runtime
            .validate_component_bytes(
                &manifest(vec![]),
                br#"(component (import "ryuzi:storage/storage@0.1.0" (instance)))"#,
                &policy,
            )
            .expect("storage import must validate once the policy grants it");
    }

    #[test]
    fn host_import_is_always_allowed_under_deny_all() {
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        runtime
            .validate_component_bytes(
                &manifest(vec![]),
                br#"(component (import "ryuzi:host/host@0.1.1" (instance)))"#,
                &HostPolicy::deny_all(),
            )
            .expect("the host-info/log import carries no secrets and is always allowed");
    }

    /// The public entrypoint `validate_component` reads the component off a
    /// [`VerifiedBundle`]'s staging directory rather than taking raw bytes —
    /// exercise that file-reading path directly, not just the
    /// bytes-in/bytes-out helper the other tests above use.
    #[test]
    fn validate_component_accepts_a_verified_bundle_staging_a_valid_component() {
        let staging_dir = tempfile::tempdir().expect("tempdir should create");
        std::fs::write(staging_dir.path().join("plugin.wasm"), b"(component)")
            .expect("writing staged component should succeed");
        let bundle = VerifiedBundle {
            manifest: manifest(vec![]),
            release: release(),
            signing_key_id: "any-key-id".to_string(),
            staging_dir: staging_dir.path().to_path_buf(),
        };

        let runtime = ComponentRuntime::new().expect("runtime should configure");
        runtime
            .validate_component(&bundle)
            .expect("a valid staged component must validate under deny-all policy");
    }

    /// Regression test: `async_support(true)` is set during engine
    /// configuration.  The test only asserts that
    /// `ComponentRuntime::new()` returns `Ok` — it does **not** fail
    /// against old code without the setting, so it does not prove absence
    /// of the regression.  It exists as a documented canary for future
    /// refactorings that might accidentally remove async support.
    #[test]
    fn new_runtime_succeeds_with_async_support_enabled() {
        let result = ComponentRuntime::new();
        assert!(
            result.is_ok(),
            "ComponentRuntime::new() must succeed with async_support enabled"
        );
    }

    /// A missing staged component file is an I/O failure to read it, not a
    /// malformed-bytes failure — `validate_component` must surface the
    /// dedicated `ComponentRead` variant, distinct from `MalformedComponent`
    /// (reserved for bytes that fail to parse as a component).
    #[test]
    fn validate_component_reports_component_read_when_staging_dir_lacks_component() {
        let staging_dir = tempfile::tempdir().expect("tempdir should create");
        // Deliberately do not write `plugin.wasm` into the staging dir.
        let bundle = VerifiedBundle {
            manifest: manifest(vec![]),
            release: release(),
            signing_key_id: "any-key-id".to_string(),
            staging_dir: staging_dir.path().to_path_buf(),
        };

        let runtime = ComponentRuntime::new().expect("runtime should configure");
        let result = runtime.validate_component(&bundle);

        let Err(error) = result else {
            panic!("a staging dir missing the component must not validate");
        };
        assert!(
            matches!(error, PluginRuntimeError::ComponentRead(_)),
            "expected ComponentRead, got {error:?}"
        );
    }
}
