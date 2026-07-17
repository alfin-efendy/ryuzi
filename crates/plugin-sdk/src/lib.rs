//! `ryuzi-plugin-sdk` — the declarative plugin contract for Ryuzi.
//!
//! This crate defines the manifest every plugin (first-party built-in,
//! embedded catalog, or user-authored) satisfies: identity and metadata, the
//! standard category vocabulary, auth description, settings fields, MCP
//! server definitions, and bundled skills. It also owns
//! the placeholder substitution grammar used to inject secrets into MCP
//! server definitions at attach time.
//!
//! It also owns the *component bundle* contract: the declarative shape of
//! a Wasm-component plugin release (`bundle` module) — identity, WIT
//! contract range, instancing lifecycle, and permission contract (network
//! allowlist, OAuth profiles) — plus the registry release descriptor for
//! it. WIT contracts themselves are not part of this crate; the bundle
//! module is data + structural validation only, with no Wasmtime or
//! installer dependency.
//!
//! Deliberately dependency-light (`serde`, `serde_json`, `toml`,
//! `thiserror`, `semver` only): this is the contract external plugin
//! authors target, and it has no opinion on how a manifest or bundle
//! becomes a running harness, gateway, connector, or Wasm component. That
//! behavioral binding lives in `ryuzi-core`'s `PluginHost`.

pub mod bundle;
pub mod categories;
pub mod manifest;
pub mod subst;

pub use bundle::{
    BundleError, NetworkPermission, OAuthProfile, PluginBundleManifest, PluginLifecycle,
    PluginPermissions, PluginRelease,
};
pub use manifest::{
    AuthKind, AuthSpec, ExtensionDef, FieldKind, ManifestError, McpServerDef, McpTransportDef,
    ModelDef, PluginManifest, ProviderMeta, SettingField, SkillDef, CONTRACT_VERSION,
    KNOWN_HOOK_EVENTS, MAX_EXTENSION_TIMEOUT_MS,
};
