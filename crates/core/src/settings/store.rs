//! Validated `SettingsStore` facade over `Store`'s raw settings K/V rows.

use crate::settings::catalog::find_field;
use crate::settings::fields::FieldType;
use crate::store::Store;
use ryuzi_plugin_sdk::{FieldKind, SettingField};
use std::collections::HashMap;
use std::sync::Arc;

/// Validate a proposed `(key, value)` setting update against the schema.
/// Returns `None` if valid, or `Some(error message)` otherwise — the exact
/// message strings are a user-visible contract covered by tests.
///
/// `plugin.*` keys are not in the static `find_field` schema (that catalog
/// is fixed at compile time; plugins are discovered at runtime) — they are
/// instead checked against `crate::plugins::plugin_field`, the process-wide
/// registry every installed plugin populates via `PluginHost::add`. An
/// unrecognized `plugin.*` key still errors "unknown setting", same as any
/// other unknown key.
pub fn validate_setting(key: &str, value: &str) -> Option<String> {
    if let Some(field) = find_field(key) {
        if field.field_type == FieldType::Enum && !field.one_of.contains(&value) {
            return Some(format!("{key} must be one of: {}", field.one_of.join(", ")));
        }
        if field.field_type == FieldType::Int
            && (value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()))
        {
            return Some(format!("{key} must be an integer"));
        }
        return None;
    }
    match crate::plugins::plugin_field(key) {
        Some(field) => validate_plugin_field(key, value, &field),
        None => Some(format!("unknown setting: {key}")),
    }
}

/// Validate a value against a plugin-declared [`SettingField`]'s `kind`.
fn validate_plugin_field(key: &str, value: &str, field: &SettingField) -> Option<String> {
    match field.kind {
        FieldKind::Bool if value != "true" && value != "false" => {
            Some(format!("{key} must be true or false"))
        }
        FieldKind::Int if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) => {
            Some(format!("{key} must be an integer"))
        }
        FieldKind::Bool | FieldKind::Int | FieldKind::String => None,
    }
}

/// Split a comma-separated string into trimmed, non-empty parts.
pub fn csv(s: Option<&str>) -> Vec<String> {
    s.unwrap_or("")
        .split(',')
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
        .collect()
}

/// Validated facade over `Store`'s raw settings rows: applies schema
/// defaults on read, validates on write, and computes required/configured
/// status from the enabled gateway/runtime provider lists.
///
/// Cheaply `Clone` (an `Arc<Store>` wrapper) so it can be handed to
/// long-lived owners like `UpdateManager` alongside other `Arc`-held deps.
#[derive(Clone)]
pub struct SettingsStore {
    store: Arc<Store>,
}

impl SettingsStore {
    pub fn new(store: Arc<Store>) -> Self {
        Self { store }
    }

    /// The persisted row, if any (even an empty string); else the field's
    /// schema default.
    pub async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        if let Some(v) = self.store.get_setting_raw(key).await? {
            return Ok(Some(v));
        }
        Ok(find_field(key).and_then(|f| f.default).map(String::from))
    }

    /// Validate then persist a setting.
    pub async fn set(&self, key: &str, value: &str) -> anyhow::Result<()> {
        if let Some(msg) = validate_setting(key, value) {
            anyhow::bail!(msg);
        }
        self.store.set_setting_raw(key, value).await
    }

    /// Persisted rows only — no schema defaults applied.
    pub async fn list(&self) -> anyhow::Result<HashMap<String, String>> {
        Ok(self.store.list_settings().await?.into_iter().collect())
    }

    /// Keys of required fields with no persisted value, in a stable order:
    /// required globals first (declaration order), then required fields of
    /// each enabled gateway (declaration order), then required fields of
    /// each enabled runtime — the order the wizard prompts in.
    pub async fn missing_required(&self) -> anyhow::Result<Vec<&'static str>> {
        use crate::settings::catalog::CATALOG;
        use crate::settings::fields::GLOBAL_FIELDS;

        let mut out = Vec::new();
        for f in GLOBAL_FIELDS {
            if f.required && self.get(f.key).await?.is_none() {
                out.push(f.key);
            }
        }
        for id in csv(self.get("enabled_gateways").await?.as_deref()) {
            if let Some(gw) = CATALOG.gateway(&id) {
                for f in gw.fields {
                    if f.required && self.get(f.key).await?.is_none() {
                        out.push(f.key);
                    }
                }
            }
        }
        for id in csv(self.get("enabled_runtimes").await?.as_deref()) {
            if let Some(rt) = CATALOG.runtime(&id) {
                for f in rt.fields {
                    if f.required && self.get(f.key).await?.is_none() {
                        out.push(f.key);
                    }
                }
            }
        }
        Ok(out)
    }

    /// At least one gateway and one runtime enabled, and no required
    /// setting is missing.
    pub async fn is_configured(&self) -> anyhow::Result<bool> {
        let gateways = csv(self.get("enabled_gateways").await?.as_deref());
        let runtimes = csv(self.get("enabled_runtimes").await?.as_deref());
        Ok(!gateways.is_empty()
            && !runtimes.is_empty()
            && self.missing_required().await?.is_empty())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::find_field;

    async fn open_test_store() -> (Store, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        (store, tmp)
    }

    #[test]
    fn validate_setting_matches_ts_messages() {
        assert_eq!(
            validate_setting("nope", "x").as_deref(),
            Some("unknown setting: nope")
        );
        assert_eq!(
            validate_setting("default_perm_mode", "bogus").as_deref(),
            Some("default_perm_mode must be one of: default, acceptEdits, bypassPermissions")
        );
        assert_eq!(
            validate_setting("max_concurrent_runs", "abc").as_deref(),
            Some("max_concurrent_runs must be an integer")
        );
        assert_eq!(
            validate_setting("max_concurrent_runs", "-1").as_deref(),
            Some("max_concurrent_runs must be an integer")
        );
        assert_eq!(validate_setting("max_concurrent_runs", "12"), None);
        assert_eq!(validate_setting("workdir_root", "/anything"), None);
        // sanity: find_field is reachable from this module too
        assert!(find_field("workdir_root").is_some());
    }

    /// Register a minimal plugin (a settings field, an auth-backed secret
    /// field, and the always-present `plugin.<id>.enabled` toggle) with a
    /// throwaway `PluginHost` — enough to exercise `validate_setting`'s
    /// `plugin.*` branch without depending on any real built-in plugin.
    fn register_test_plugin(id: &str) {
        use crate::plugins::{CorePlugin, PluginHost, PluginSource};
        use ryuzi_plugin_sdk::{AuthKind, AuthSpec, FieldKind, PluginManifest, SettingField};

        let manifest = PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: format!("Test Plugin {id}"),
            version: String::new(),
            publisher: String::new(),
            description: String::new(),
            homepage: None,
            icon: None,
            categories: vec![],
            verified: false,
            experimental: false,
            auth: Some(AuthSpec {
                kind: AuthKind::Token,
                setting: Some(format!("plugin.{id}.token")),
                env: None,
                help_url: None,
            }),
            settings: vec![SettingField {
                key: format!("plugin.{id}.host"),
                label: "Host".to_string(),
                help: String::new(),
                secret: false,
                required: false,
                kind: FieldKind::String,
            }],
            mcp: vec![],
            skills: vec![],
            menu: None,
            provider: None,
            runtime: None,
        };
        let mut host = PluginHost::new();
        host.add(CorePlugin {
            manifest,
            harness: None,
            gateway: None,
            connector: None,
            source: PluginSource::Builtin,
        });
    }

    #[test]
    fn validate_setting_accepts_registered_plugin_fields_and_rejects_unknown_plugin_keys() {
        // Unique id: `PluginHost::add`'s `plugin_field` registration is
        // process-wide, so a shared id could pick up state from another
        // test's plugin running in the same test binary.
        let id = "task7-storetest-plugin";
        register_test_plugin(id);

        // A custom `manifest.settings[]` field: any string value is fine.
        assert_eq!(
            validate_setting(&format!("plugin.{id}.host"), "example.com"),
            None
        );
        // The auth-backed secret field, registered from `manifest.auth.setting`.
        assert_eq!(
            validate_setting(&format!("plugin.{id}.token"), "sekret"),
            None
        );
        // `plugin.<id>.enabled` is always registered, as a Bool field.
        assert_eq!(
            validate_setting(&format!("plugin.{id}.enabled"), "true"),
            None
        );
        assert_eq!(
            validate_setting(&format!("plugin.{id}.enabled"), "false"),
            None
        );
        assert_eq!(
            validate_setting(&format!("plugin.{id}.enabled"), "maybe").as_deref(),
            Some(format!("plugin.{id}.enabled must be true or false").as_str())
        );
        // Never registered for this plugin — still an unknown setting.
        assert_eq!(
            validate_setting(&format!("plugin.{id}.nope"), "x").as_deref(),
            Some(format!("unknown setting: plugin.{id}.nope").as_str())
        );
        // A `plugin.*`-shaped key belonging to no installed plugin at all.
        assert_eq!(
            validate_setting("plugin.totally-unregistered-plugin.enabled", "true").as_deref(),
            Some("unknown setting: plugin.totally-unregistered-plugin.enabled")
        );
    }

    #[test]
    fn csv_trims_and_drops_empties() {
        assert_eq!(csv(Some("a, b,,c ")), vec!["a", "b", "c"]);
        assert!(csv(None).is_empty());
        assert!(csv(Some("")).is_empty());
    }

    #[tokio::test]
    async fn settings_store_defaults_validation_and_missing() {
        let (store, _tmp) = open_test_store().await; // shared helper from store.rs tests — move/duplicate a minimal opener here
        let settings = SettingsStore::new(std::sync::Arc::new(store));
        // default fallback (not persisted):
        assert_eq!(
            settings.get("default_effort").await.unwrap().as_deref(),
            Some("medium")
        );
        assert!(!settings
            .list()
            .await
            .unwrap()
            .contains_key("default_effort"));
        // set validates:
        assert!(settings.set("default_perm_mode", "bogus").await.is_err());
        settings
            .set("default_perm_mode", "acceptEdits")
            .await
            .unwrap();
        // fresh-db missing list, exact order (seeds enable discord):
        assert_eq!(
            settings.missing_required().await.unwrap(),
            vec![
                "workdir_root",
                "discord.token",
                "discord.app_id",
                "discord.guild_id"
            ]
        );
        // empty string counts as set:
        settings.set("workdir_root", "").await.unwrap();
        assert!(!settings
            .missing_required()
            .await
            .unwrap()
            .contains(&"workdir_root"));
        assert!(!settings.is_configured().await.unwrap());
        for (k, v) in [
            ("discord.token", "t"),
            ("discord.app_id", "a"),
            ("discord.guild_id", "g"),
        ] {
            settings.set(k, v).await.unwrap();
        }
        assert!(settings.is_configured().await.unwrap());
    }
}
