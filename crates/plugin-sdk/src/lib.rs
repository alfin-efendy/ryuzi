//! `ryuzi-plugin-sdk` — the declarative plugin contract for Ryuzi.
//!
//! This crate defines the manifest every plugin (first-party built-in,
//! embedded catalog, or user-authored) satisfies: identity and metadata, the
//! standard category vocabulary, auth description, settings fields, MCP
//! server definitions, and bundled skills. It also owns
//! the placeholder substitution grammar used to inject secrets into MCP
//! server definitions at attach time.
//!
//! Deliberately dependency-light (`serde`, `serde_json`, `toml`,
//! `thiserror` only): this is the contract external plugin authors target,
//! and it has no opinion on how a manifest becomes a running harness,
//! gateway, or connector. That behavioral binding lives in `ryuzi-core`'s
//! `PluginHost`.

pub mod categories;
pub mod manifest;
pub mod subst;

pub use manifest::{
    AuthKind, AuthSpec, ExtensionDef, FieldKind, ManifestError, McpServerDef, McpTransportDef,
    ModelDef, PluginManifest, ProviderMeta, SettingField, SkillDef, CONTRACT_VERSION,
    KNOWN_HOOK_EVENTS, MAX_EXTENSION_TIMEOUT_MS,
};
