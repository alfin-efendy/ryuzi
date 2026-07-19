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
use crate::plugins::capabilities::settings::{ScopedSettings, SettingsErr};
use crate::plugins::capabilities::storage::{PluginStorage, StorageErr};
use crate::plugins::capabilities::wit_bindings::ryuzi::host::host as host_iface;
use crate::plugins::capabilities::wit_bindings::ryuzi::http::http as http_iface;
use crate::plugins::capabilities::wit_bindings::ryuzi::oauth::oauth as oauth_iface;
use crate::plugins::capabilities::wit_bindings::ryuzi::settings::settings as settings_iface;
use crate::plugins::capabilities::wit_bindings::ryuzi::storage::storage as storage_iface;
use crate::plugins::capabilities::PluginCapabilityContext;
use ryuzi_plugin_sdk::PluginBundleManifest;
use std::sync::Arc;
use wasmtime::{
    component::{Component, HasSelf, Linker},
    Config, Engine, Store,
};

const HTTP_IMPORT: &str = "ryuzi:http/http@0.1.0";
const SETTINGS_IMPORT: &str = "ryuzi:settings/settings@0.1.0";
const STORAGE_IMPORT: &str = "ryuzi:storage/storage@0.1.0";
const HOST_IMPORT: &str = "ryuzi:host/host@0.1.1";
const OAUTH_IMPORT: &str = "ryuzi:oauth/oauth@0.2.0";
const TYPES_IMPORT: &str = "ryuzi:plugin/types@0.1.0";
const LIFECYCLE_EXPORT: &str = "ryuzi:plugin/lifecycle@0.1.0";
const ALLOWED_EXPORTS: &[&str] = &[
    "lifecycle",
    LIFECYCLE_EXPORT,
    "ryuzi:gateway/gateway@0.1.0",
    "ryuzi:connector/connector@0.1.0",
    "ryuzi:provider/provider@0.1.0",
    "ryuzi:hooks/hooks@0.1.0",
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

/// Validates a WebAssembly component before later runtime layers link it.
pub struct ComponentRuntime {
    engine: Engine,
}

impl ComponentRuntime {
    pub fn new() -> Result<Self, PluginRuntimeError> {
        let mut config = Config::new();
        config.wasm_component_model(true);
        config.consume_fuel(true);
        config.epoch_interruption(true);
        // Wasmtime 46 always enables async support, but task 6 requires this
        // explicit configuration call.  Narrow suppression keeps clippy clean
        // without widening to module or crate scope.
        #[allow(deprecated)]
        config.async_support(true);
        let engine = Engine::new(&config)
            .map_err(|error| PluginRuntimeError::EngineInitialization(error.to_string()))?;
        Ok(Self { engine })
    }

    fn validate_component_bytes(
        &self,
        manifest: &PluginBundleManifest,
        bytes: &[u8],
        policy: &HostPolicy,
    ) -> Result<Component, PluginRuntimeError> {
        let component = Component::new(&self.engine, bytes)
            .map_err(|error| PluginRuntimeError::MalformedComponent(error.to_string()))?;
        for (name, _) in component.component_type().imports(&self.engine) {
            let is_wasi_baseline = name.starts_with("wasi:");
            let network_is_authorized = name == HTTP_IMPORT
                && !manifest.permissions.network.is_empty()
                && policy.allow_network;
            let types_is_authorized = name == TYPES_IMPORT;
            let host_is_authorized = name == HOST_IMPORT;
            let settings_is_authorized = name == SETTINGS_IMPORT && policy.allow_settings;
            let storage_is_authorized = name == STORAGE_IMPORT && policy.allow_storage;
            let oauth_is_authorized = name == OAUTH_IMPORT && policy.allow_oauth;
            if !is_wasi_baseline
                && !types_is_authorized
                && !network_is_authorized
                && !host_is_authorized
                && !settings_is_authorized
                && !storage_is_authorized
                && !oauth_is_authorized
            {
                let reason = if name == HTTP_IMPORT {
                    "network requires a manifest allowlist and host policy approval".to_string()
                } else if name == SETTINGS_IMPORT {
                    "settings access requires host policy approval".to_string()
                } else if name == STORAGE_IMPORT {
                    "storage access requires host policy approval".to_string()
                } else if name == OAUTH_IMPORT {
                    "OAuth access requires host policy approval".to_string()
                } else {
                    "no host capability is enabled by this runtime slice".to_string()
                };
                return Err(PluginRuntimeError::DeniedImport {
                    name: name.to_string(),
                    reason,
                });
            }
        }
        for (name, _) in component.component_type().exports(&self.engine) {
            if !ALLOWED_EXPORTS.contains(&name) {
                return Err(PluginRuntimeError::DeniedExport {
                    name: name.to_string(),
                    reason: "not declared by the ryuzi:plugin@0.1.0 world".to_string(),
                });
            }
        }
        Ok(component)
    }

    /// Validates the component staged by a signed bundle under deny-all policy.
    pub fn validate_component(&self, bundle: &VerifiedBundle) -> Result<(), PluginRuntimeError> {
        let bytes = std::fs::read(bundle.staging_dir.join(&bundle.manifest.component))
            .map_err(|error| PluginRuntimeError::ComponentRead(error.to_string()))?;
        self.validate_component_bytes(&bundle.manifest, &bytes, &HostPolicy::deny_all())
            .map(|_| ())
    }

    /// Instantiates a component after policy validation, linking the host
    /// capability adapters (`ryuzi:host/host` always; `ryuzi:settings/settings`
    /// and `ryuzi:storage/storage` only when `policy` grants them) into the
    /// linker before instantiation.
    ///
    /// Timeout enforcement uses epoch interruption on a blocking thread:
    /// synchronous `Linker::instantiate` runs inside `spawn_blocking`, and a
    /// `tokio::select!` races it against the deadline.  When the timer fires
    /// first we call `engine.increment_epoch()` once so the CPU-bound Wasm
    /// sees its epoch deadline and traps with an interrupt.  The join handle
    /// is never detached — we always await it.
    pub async fn instantiate(
        &self,
        bundle: &InstalledBundle,
        policy: HostPolicy,
        ctx: Arc<PluginCapabilityContext>,
    ) -> Result<(), PluginRuntimeError> {
        let bytes = std::fs::read(&bundle.component_path)
            .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
        let component = self.validate_component_bytes(&bundle.manifest, &bytes, &policy)?;

        // Clone the engine so the spawned task owns everything it needs
        // without borrowing `self`.
        let engine = self.engine.clone();
        let fuel = policy.limits.fuel;
        let timeout = policy.limits.timeout;
        let allow_network = policy.allow_network;
        let allow_settings = policy.allow_settings;
        let allow_storage = policy.allow_storage;
        let allow_oauth = policy.allow_oauth;
        // Built from the manifest's own declared network permissions (not
        // policy-conditioned here — the import is only linked at all when
        // `allow_network` is true, and `validate_component_bytes` already
        // requires a non-empty manifest allowlist for the import to be
        // authorized in the first place).
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
        let runtime_ctx = Arc::new(PluginCapabilityContext {
            plugin_id: bundle.manifest.id.clone(),
            version: bundle.manifest.version.clone(),
            settings: ctx.settings.clone(),
            store: ctx.store.clone(),
            telemetry: ctx.telemetry.clone(),
            network_allowlist: network_allowlist.clone(),
            oauth_profile_ids: oauth_profile_ids.clone(),
        });
        // Captured on the async caller's thread — inside `spawn_blocking`
        // there is no ambient Tokio reactor, so the sync `Host` trait impls
        // bridge back to it explicitly via `Handle::block_on`.
        let rt = tokio::runtime::Handle::current();

        // Run synchronous instantiation on a blocking thread so that
        // tokio::select! can race it against a sleep timer.  A plain
        // tokio::time::timeout around await cannot preempt CPU-bound
        // Wasm; epoch interruption on a blocking thread is the correct
        // mechanism.
        let join_handle = tokio::task::spawn_blocking(move || {
            let state = CapabilityState {
                ctx: runtime_ctx,
                allow_network,
                network_allowlist,
                rt,
            };
            let mut store = Store::new(&engine, state);
            store
                .set_fuel(fuel)
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            store.set_epoch_deadline(1);
            let mut linker = Linker::new(&engine);
            // `host` carries no secrets and has no side effects — always
            // linked regardless of policy.
            host_iface::add_to_linker_instance::<CapabilityState, HasSelf<CapabilityState>>(
                &mut linker
                    .instance("host")
                    .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?,
                |s: &mut CapabilityState| s,
            )
            .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            if allow_settings {
                settings_iface::add_to_linker_instance::<CapabilityState, HasSelf<CapabilityState>>(
                    &mut linker.instance("settings").map_err(|error| {
                        PluginRuntimeError::InstantiationFailed(error.to_string())
                    })?,
                    |s: &mut CapabilityState| s,
                )
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            }
            if allow_storage {
                storage_iface::add_to_linker_instance::<CapabilityState, HasSelf<CapabilityState>>(
                    &mut linker.instance("storage").map_err(|error| {
                        PluginRuntimeError::InstantiationFailed(error.to_string())
                    })?,
                    |s: &mut CapabilityState| s,
                )
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            }
            if allow_network {
                http_iface::add_to_linker_instance::<CapabilityState, HasSelf<CapabilityState>>(
                    &mut linker.instance("http").map_err(|error| {
                        PluginRuntimeError::InstantiationFailed(error.to_string())
                    })?,
                    |s: &mut CapabilityState| s,
                )
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            }
            if allow_oauth {
                oauth_iface::add_to_linker_instance::<CapabilityState, HasSelf<CapabilityState>>(
                    &mut linker.instance("oauth").map_err(|error| {
                        PluginRuntimeError::InstantiationFailed(error.to_string())
                    })?,
                    |s: &mut CapabilityState| s,
                )
                .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
            }
            linker
                .instantiate(&mut store, &component)
                .map(|_instance| ())
                .map_err(|error| match error.downcast_ref::<wasmtime::Trap>() {
                    Some(wasmtime::Trap::OutOfFuel) => {
                        PluginRuntimeError::FuelExhausted(error.to_string())
                    }
                    _ => PluginRuntimeError::InstantiationFailed(error.to_string()),
                })
        });

        tokio::pin!(join_handle);

        tokio::select! {
            result = &mut join_handle => {
                // The blocking task completed within the deadline.
                match result {
                    Ok(inner) => inner,
                    Err(join_error) => Err(PluginRuntimeError::InstantiationFailed(format!(
                        "instantiation task panicked: {join_error}"
                    ))),
                }
            }
            _ = tokio::time::sleep(timeout) => {
                // The timer fired first.  Increment the epoch exactly once
                // so the CPU-bound Wasm sees its epoch deadline and traps
                // with an interrupt.
                self.engine.increment_epoch();

                // Await the blocking task so we never detach a background
                // thread.  The Wasm should now exit quickly via the epoch
                // trap.  Any result after the timer wins is mapped to
                // TimeoutExceeded because the operation exceeded its host
                // deadline.
                match join_handle.await {
                    Ok(Ok(())) | Ok(Err(_)) => {
                        Err(PluginRuntimeError::TimeoutExceeded { timeout })
                    }
                    Err(join_error) => Err(PluginRuntimeError::InstantiationFailed(format!(
                        "instantiation task panicked: {join_error}"
                    ))),
                }
            }
        }
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
struct CapabilityState {
    ctx: Arc<PluginCapabilityContext>,
    allow_network: bool,
    /// This plugin's bundle-declared network allowlist entries (bare
    /// hostnames or `*.`-prefixed wildcards). Populated by `instantiate`
    /// from `bundle.manifest.permissions.network` regardless of whether
    /// `allow_network` is true — the `http_iface::Host::request` impl below
    /// only ever consults it when `allow_network` holds, since the `http`
    /// instance is not even linked otherwise.
    network_allowlist: Vec<String>,
    rt: tokio::runtime::Handle,
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
        let client = AllowedHttpClient::new(allowlist);
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
        let result = self.rt.block_on(async move {
            ProfileOauth::new(&ctx)
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
                _ => panic!("unknown fixture {name}"),
            })
    }

    fn build_fixture_components() {
        let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let script = root
            .join("tests")
            .join("fixtures")
            .join("build-components.sh");
        let status = std::process::Command::new("sh")
            .arg(script)
            .status()
            .expect("fixture build script should start");
        assert!(
            status.success(),
            "fixture build script failed with {status}"
        );
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
