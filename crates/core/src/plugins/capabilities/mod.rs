//! WASM Component Model host-capability adapters: bridges declared plugin
//! capabilities (host info, scoped settings, scoped storage) to the
//! generated `ryuzi:plugin` world's WIT host imports.
//!
//! # Layout
//! - [`wit_bindings`] — the one `wasmtime::component::bindgen!` invocation
//!   for the whole crate; every other module in here reuses its generated
//!   types instead of re-deriving them.
//! - [`host`] — `ryuzi:host/host`: plugin identity + granted capability
//!   flags. Always linked (see `runtime::instantiate`) — it carries no
//!   secrets and no side effects.
//! - [`settings`] — `ryuzi:settings/settings`: a plugin's own `plugin.<id>.*`
//!   settings slice, scoped so cross-plugin access is structurally
//!   impossible (see `settings`'s module doc for the exact convention).
//! - [`storage`] — `ryuzi:storage/storage`: a plugin's own key/value rows in
//!   `component_plugin_storage`, scoped by `plugin_id` at the `Store` layer.
//!
//! [`PluginCapabilityContext`] is the one value every adapter borrows from —
//! it identifies which plugin is calling and gives access to the shared
//! `SettingsStore`/`Store`/`Telemetry` the host process already owns.

pub mod host;
pub mod http;
pub mod settings;
pub mod storage;
pub mod wit_bindings;

use std::sync::Arc;

use crate::settings::SettingsStore;
use crate::store::Store;
use crate::telemetry::Telemetry;

/// Everything a capability adapter needs to serve one plugin's WIT host
/// calls: which plugin it is, and shared handles to the settings/storage/
/// telemetry backends those calls are scoped against.
pub struct PluginCapabilityContext {
    pub plugin_id: String,
    pub version: String,
    pub settings: SettingsStore,
    pub store: Arc<Store>,
    pub telemetry: Arc<dyn Telemetry>,
}

/// Case-insensitive substrings that mark a field *name* as secret-shaped —
/// mirrors `crate::plugins::extension::events::SECRET_SHAPED_MARKERS`
/// (kept as a separate copy: that list screens free-form deny-reason
/// *text*, this one screens setting/log *field names*, and the two lists
/// are allowed to diverge independently over time).
const SECRET_SHAPED_MARKERS: &[&str] = &[
    "authorization",
    "bearer",
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "api_key",
    "api-key",
    "credential",
];

/// Whether `name` looks like it names a secret, purely from its spelling —
/// deliberately broad and over-inclusive (see the marker list's own doc for
/// the false-positive/false-negative tradeoff this makes).
pub fn is_secret_shaped_field(name: &str) -> bool {
    let lower = name.to_lowercase();
    SECRET_SHAPED_MARKERS.iter().any(|m| lower.contains(m))
}

/// Redact `value` to `"[redacted]"` when `name` either looks secret-shaped
/// ([`is_secret_shaped_field`]) or is a settings key the schema/plugin
/// manifest declares secret (`crate::settings::is_secret`); otherwise
/// returns `value` unchanged. Used anywhere a capability adapter might log
/// or surface a field it did not itself choose to treat as sensitive.
pub fn redact_log_field(name: &str, value: &str) -> String {
    if is_secret_shaped_field(name) || crate::settings::is_secret(name) {
        "[redacted]".to_string()
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_shaped_names_are_redacted() {
        for name in [
            "Authorization",
            "authorization",
            "X-Auth-Token",
            "api_secret",
            "password",
        ] {
            assert_eq!(
                redact_log_field(name, "sensitive-value"),
                "[redacted]",
                "expected {name} to be treated as secret-shaped"
            );
        }
    }

    #[test]
    fn a_declared_secret_setting_key_is_redacted_even_without_a_secret_shaped_name() {
        // Unique id: `plugin_field` is a process-wide registry (see
        // `crate::plugins::host`), so a shared id could collide with
        // another test's plugin in the same test binary.
        let id = "task7-capredact-plugin";
        let manifest = ryuzi_plugin_sdk::PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: "Redaction Test Plugin".to_string(),
            version: String::new(),
            publisher: String::new(),
            description: String::new(),
            homepage: None,
            icon: None,
            categories: vec![],
            slot: None,
            verified: false,
            experimental: false,
            auth: Some(ryuzi_plugin_sdk::AuthSpec {
                kind: ryuzi_plugin_sdk::AuthKind::Token,
                // Deliberately NOT a secret-shaped name (no "token",
                // "secret", etc. substring) — this proves redaction fires
                // from the declared-secret schema lookup, not the name
                // heuristic.
                setting: Some(format!("plugin.{id}.value")),
                ..Default::default()
            }),
            settings: vec![],
            mcp: vec![],
            extensions: vec![],
            skills: vec![],
            provider: None,
        };
        let mut host = crate::plugins::PluginHost::new();
        host.add(crate::plugins::CorePlugin {
            manifest,
            harness: None,
            gateway: None,
            connector: None,
            extension: None,
            source: crate::plugins::PluginSource::Builtin,
        });

        let key = format!("plugin.{id}.value");
        assert!(
            !is_secret_shaped_field(&key),
            "sanity: the key name itself must not be secret-shaped"
        );
        assert_eq!(redact_log_field(&key, "sekret-value"), "[redacted]");
    }

    #[test]
    fn a_plain_field_passes_through_unchanged() {
        assert_eq!(redact_log_field("region", "us-east-1"), "us-east-1");
    }
}
