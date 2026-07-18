//! Component Model runtime validation.
//!
//! This first runtime slice intentionally grants no host capabilities. It
//! parses a component and rejects every import except a network import that is
//! declared in the bundle manifest and allowed by host policy. Capability
//! linking and component execution follow in later runtime slices.

use std::fmt;
use std::time::Duration;

use crate::plugins::bundle::{InstalledBundle, VerifiedBundle};
use ryuzi_plugin_sdk::PluginBundleManifest;
use wasmtime::{
    component::{Component, Linker},
    Config, Engine, Store,
};

const HTTP_IMPORT: &str = "ryuzi:http/http@0.1.0";
const ALLOWED_EXPORTS: &[&str] = &[
    "lifecycle",
    "ryuzi:gateway/gateway@0.1.0",
    "ryuzi:connector/connector@0.1.0",
    "ryuzi:provider/provider@0.1.0",
    "ryuzi:hooks/hooks@0.1.0",
];

/// Default resource budget a plugin runtime may consume.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceLimits {
    pub max_memory_bytes: u64,
    pub fuel: u64,
    pub timeout: Duration,
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
    pub limits: ResourceLimits,
}

impl HostPolicy {
    /// The default policy: no component receives host capabilities.
    pub fn deny_all() -> Self {
        Self {
            allow_network: false,
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
            let network_is_authorized = name == HTTP_IMPORT
                && !manifest.permissions.network.is_empty()
                && policy.allow_network;
            if !network_is_authorized {
                let reason = if name == HTTP_IMPORT {
                    "network requires a manifest allowlist and host policy approval".to_string()
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

    /// Instantiates an import-free component after policy validation. Capability
    /// linker definitions are intentionally deferred to the host-adapter slice.
    pub async fn instantiate(
        &self,
        bundle: &InstalledBundle,
        policy: HostPolicy,
    ) -> Result<(), PluginRuntimeError> {
        let bytes = std::fs::read(&bundle.component_path)
            .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
        let component = self.validate_component_bytes(&bundle.manifest, &bytes, &policy)?;
        let mut store = Store::new(&self.engine, ());
        store
            .set_fuel(policy.limits.fuel)
            .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
        store.set_epoch_deadline(1);
        let linker = Linker::new(&self.engine);
        linker
            .instantiate(&mut store, &component)
            .map_err(|error| PluginRuntimeError::InstantiationFailed(error.to_string()))?;
        Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::ComponentPluginReleaseRecord;
    use ryuzi_plugin_sdk::{NetworkPermission, PluginLifecycle, PluginPermissions, PluginRelease};

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

    #[tokio::test]
    async fn instantiate_succeeds_for_an_installed_component_under_deny_all() {
        let dir = tempfile::tempdir().expect("tempdir should create");
        let runtime = ComponentRuntime::new().expect("runtime should configure");
        runtime
            .instantiate(&installed_bundle(dir.path()), HostPolicy::deny_all())
            .await
            .expect("an import-free installed component should instantiate");
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
