//! Data-only provider catalog: gateway descriptors and their
//! provider-specific `ConfigField`s. Field keys are user-visible contracts —
//! settings stored under these keys must keep resolving across releases.

use crate::settings::fields::{ConfigField, GLOBAL_FIELDS};

pub struct GatewayDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub fields: &'static [ConfigField],
}

pub struct ProviderCatalog {
    pub gateways: &'static [GatewayDescriptor],
}

impl ProviderCatalog {
    pub fn gateway(&self, id: &str) -> Option<&'static GatewayDescriptor> {
        self.gateways.iter().find(|g| g.id == id)
    }
}

/// Native (in-process) gateway descriptors. Discord — the only historical
/// entry — migrated to a signed WASM component bundle (which declares its own
/// settings in its manifest), so this is empty today. The
/// `GatewayDescriptor`/`ProviderCatalog` machinery and the `enabled_gateways`
/// settings plumbing are retained as generic infrastructure a future native
/// gateway would populate.
pub static CATALOG: ProviderCatalog = ProviderCatalog { gateways: &[] };

/// All fields in schema order: globals, then each gateway's fields.
pub fn all_fields() -> Vec<&'static ConfigField> {
    GLOBAL_FIELDS
        .iter()
        .chain(CATALOG.gateways.iter().flat_map(|g| g.fields.iter()))
        .collect()
}

pub fn find_field(key: &str) -> Option<&'static ConfigField> {
    all_fields().into_iter().find(|f| f.key == key)
}

/// Whether `key` is a secret — either a static `ConfigField` marked secret,
/// or a `plugin.*` field a plugin's manifest declared secret (see
/// `crate::plugins::plugin_field`).
pub fn is_secret(key: &str) -> bool {
    if let Some(f) = find_field(key) {
        return f.secret;
    }
    crate::plugins::plugin_field(key)
        .map(|f| f.secret)
        .unwrap_or(false)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::plugins::{CorePlugin, PluginHost, PluginSource};
    use ryuzi_plugin_sdk::{AuthKind, AuthSpec, PluginManifest};

    #[test]
    fn is_secret_true_for_a_registered_secret_plugin_field_false_for_its_other_keys() {
        // Unique id — `plugin_field` is a process-wide registry (see
        // `crate::plugins::host`), so a shared id could collide with another
        // test's plugin in the same test binary.
        let id = "task7-catalogtest-plugin";
        let manifest = PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: "Catalog Test Plugin".to_string(),
            version: String::new(),
            publisher: String::new(),
            description: String::new(),
            homepage: None,
            icon: None,
            categories: vec![],
            slot: None,
            verified: false,
            experimental: false,
            auth: Some(AuthSpec {
                kind: AuthKind::Token,
                setting: Some(format!("plugin.{id}.token")),
                ..Default::default()
            }),
            settings: vec![],
            mcp: vec![],
            extensions: vec![],
            skills: vec![],
            provider: None,
        };
        let mut host = PluginHost::new();
        host.add(CorePlugin {
            manifest,
            harness: None,
            gateway: None,
            connector: None,
            extension: None,
            provider: None,
            source: PluginSource::Builtin,
        });

        assert!(
            is_secret(&format!("plugin.{id}.token")),
            "auth.setting-backed fields are always registered as secret"
        );
        assert!(
            !is_secret(&format!("plugin.{id}.enabled")),
            "the enabled toggle is a plain Bool, not a secret"
        );
        assert!(!is_secret(&format!("plugin.{id}.nope")));
        assert!(!is_secret("workdir_root"));
    }
}
