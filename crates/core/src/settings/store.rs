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
    // `models.meta.<model_id>` is a dynamic-suffix key (like `plugin.*`):
    // per-model metadata overrides carrying a JSON object value.
    if let Some(model_id) = key.strip_prefix("models.meta.") {
        if model_id.is_empty() {
            return Some(format!("unknown setting: {key}"));
        }
        return match serde_json::from_str::<serde_json::Value>(value) {
            Ok(v) if v.is_object() => None,
            _ => Some(format!("{key} must be a JSON object")),
        };
    }
    match crate::plugins::plugin_field(key) {
        Some(field) => validate_plugin_field(key, value, &field),
        None => Some(format!("unknown setting: {key}")),
    }
}

/// Validate a value against a plugin-declared [`SettingField`]'s `kind` and,
/// when the field is an enum (`options` non-empty), against its members.
fn validate_plugin_field(key: &str, value: &str, field: &SettingField) -> Option<String> {
    if !field.options.is_empty() && !field.options.iter().any(|o| o == value) {
        return Some(format!(
            "{key} must be one of: {}",
            field.options.join(", ")
        ));
    }
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

    pub(crate) fn store(&self) -> Arc<Store> {
        self.store.clone()
    }

    /// The persisted row, if any (even an empty string); else the field's
    /// schema default — the static `find_field` catalog's default for a
    /// global/gateway key, or (for a key not in that compile-time catalog)
    /// the `default` a plugin declared on its `manifest.settings[]` field via
    /// `crate::plugins::plugin_field`. Precedence: explicit stored value >
    /// static catalog default > plugin field default > `None`. This is the
    /// single read path `${setting:KEY}` substitution and required-field
    /// checks (`declarative.rs::ensure_auth`, `missing_required` below) go
    /// through, so a manifest `default` now actually takes effect instead of
    /// only ever appearing as Cockpit placeholder text.
    pub async fn get(&self, key: &str) -> anyhow::Result<Option<String>> {
        if let Some(v) = self.store.get_setting_raw(key).await? {
            return Ok(Some(v));
        }
        if let Some(default) = find_field(key).and_then(|f| f.default) {
            return Ok(Some(default.to_string()));
        }
        Ok(crate::plugins::plugin_field(key).and_then(|f| f.default))
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
    /// each enabled gateway (declaration order) — the order the wizard
    /// prompts in — then required plugin-declared `manifest.settings[]`
    /// fields of every *enabled* plugin (sorted, since the backing registry
    /// is a `HashMap`).
    ///
    /// Plugin fields return owned `String`s (unlike the `&'static str` global
    /// and gateway keys) because they come from a runtime manifest, not a
    /// compile-time schema.
    pub async fn missing_required(&self) -> anyhow::Result<Vec<String>> {
        use crate::settings::catalog::CATALOG;
        use crate::settings::fields::GLOBAL_FIELDS;

        let mut out = Vec::new();
        for f in GLOBAL_FIELDS {
            if f.required && self.get(f.key).await?.is_none() {
                out.push(f.key.to_string());
            }
        }
        for id in csv(self.get("enabled_gateways").await?.as_deref()) {
            if let Some(gw) = CATALOG.gateway(&id) {
                for f in gw.fields {
                    if f.required && self.get(f.key).await?.is_none() {
                        out.push(f.key.to_string());
                    }
                }
            }
        }
        // Gated on the raw `plugin.<id>.enabled` setting — the same key
        // `PluginHost::is_enabled`'s connector-only branch reads (mirrored
        // rather than called: this facade never holds a
        // `PluginHost`/`Registries` handle), so a disabled connector
        // plugin's required fields don't block onboarding/`is_configured()`
        // forever. This key is never set for gateway-capable plugins (their
        // `is_enabled` reads `enabled_gateways` instead), so their fields —
        // already covered by the gateway loop above — are correctly skipped
        // here rather than double-counted. Harness-capable and manifest-only
        // plugins (always-enabled regardless of this key) declare no
        // required custom settings fields today; if one ever does, it would
        // need `plugin.<id>.enabled=true` set explicitly for this loop to
        // see it as enabled — a known simplification.
        let mut plugin_required: Vec<(String, String)> = crate::plugins::plugin_fields_all()
            .into_iter()
            .filter(|(_, f)| f.required)
            .map(|(plugin_id, f)| (plugin_id, f.key))
            .collect();
        plugin_required.sort();
        for (plugin_id, key) in plugin_required {
            let enabled_key = format!("plugin.{plugin_id}.enabled");
            if self.get(&enabled_key).await?.as_deref() != Some("true") {
                continue;
            }
            if self.get(&key).await?.is_none() {
                out.push(key);
            }
        }
        Ok(out)
    }

    /// At least one gateway enabled, and no required setting is missing.
    pub async fn is_configured(&self) -> anyhow::Result<bool> {
        let gateways = csv(self.get("enabled_gateways").await?.as_deref());
        Ok(!gateways.is_empty() && self.missing_required().await?.is_empty())
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
            slot: None,
            verified: false,
            experimental: false,
            auth: Some(AuthSpec {
                kind: AuthKind::Token,
                setting: Some(format!("plugin.{id}.token")),
                ..Default::default()
            }),
            settings: vec![SettingField {
                key: format!("plugin.{id}.host"),
                label: "Host".to_string(),
                help: String::new(),
                secret: false,
                required: false,
                kind: FieldKind::String,
                options: Vec::new(),
                default: None,
            }],
            mcp: vec![],
            skills: vec![],
            provider: None,
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

    /// Register a plugin with one enum settings field (`options` non-empty)
    /// — the `required` flag is parameterized so the same helper backs both
    /// the enum-rejection test and the required-plugin-field enforcement
    /// test.
    fn register_enum_plugin(id: &str, required: bool) {
        use crate::plugins::{CorePlugin, PluginHost, PluginSource};
        use ryuzi_plugin_sdk::{FieldKind, PluginManifest, SettingField};

        let manifest = PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: format!("Test Enum Plugin {id}"),
            version: String::new(),
            publisher: String::new(),
            description: String::new(),
            homepage: None,
            icon: None,
            categories: vec![],
            slot: None,
            verified: false,
            experimental: false,
            auth: None,
            settings: vec![SettingField {
                key: format!("plugin.{id}.tier"),
                label: "Tier".to_string(),
                help: String::new(),
                secret: false,
                required,
                kind: FieldKind::String,
                options: vec!["free".to_string(), "pro".to_string()],
                default: None,
            }],
            mcp: vec![],
            skills: vec![],
            provider: None,
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
    fn validate_setting_rejects_a_value_outside_a_registered_plugin_enum_options() {
        let id = "task-c3-storetest-enum-plugin";
        register_enum_plugin(id, false);

        assert_eq!(validate_setting(&format!("plugin.{id}.tier"), "free"), None);
        assert_eq!(validate_setting(&format!("plugin.{id}.tier"), "pro"), None);
        assert_eq!(
            validate_setting(&format!("plugin.{id}.tier"), "ultra").as_deref(),
            Some(format!("plugin.{id}.tier must be one of: free, pro").as_str())
        );
    }

    #[tokio::test]
    async fn missing_required_includes_a_required_plugin_field_only_once_the_plugin_is_enabled() {
        let id = "task-c3-storetest-required-plugin";
        register_enum_plugin(id, true);
        let key = format!("plugin.{id}.tier");

        let (store, _tmp) = open_test_store().await;
        let settings = SettingsStore::new(std::sync::Arc::new(store));

        // Disabled by default: the required field must not block onboarding
        // even though it's unset.
        assert!(!settings
            .missing_required()
            .await
            .unwrap()
            .iter()
            .any(|k| k == &key));

        // Enabling the plugin surfaces its unset required field as missing.
        settings
            .set(&format!("plugin.{id}.enabled"), "true")
            .await
            .unwrap();
        assert!(settings
            .missing_required()
            .await
            .unwrap()
            .iter()
            .any(|k| k == &key));

        // Setting a valid member value clears it.
        settings.set(&key, "free").await.unwrap();
        assert!(!settings
            .missing_required()
            .await
            .unwrap()
            .iter()
            .any(|k| k == &key));
    }

    /// Register a plugin with two settings fields: one carrying a `default`
    /// (`required` parameterized) and a sibling with no `default`, to prove
    /// the read-path fallback (this fix) is conditional on the field
    /// actually declaring one.
    fn register_plugin_with_default(id: &str, required: bool) {
        use crate::plugins::{CorePlugin, PluginHost, PluginSource};
        use ryuzi_plugin_sdk::{FieldKind, PluginManifest, SettingField};

        let manifest = PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: format!("Test Default Plugin {id}"),
            version: String::new(),
            publisher: String::new(),
            description: String::new(),
            homepage: None,
            icon: None,
            categories: vec![],
            slot: None,
            verified: false,
            experimental: false,
            auth: None,
            settings: vec![
                SettingField {
                    key: format!("plugin.{id}.tier"),
                    label: "Tier".to_string(),
                    help: String::new(),
                    secret: false,
                    required,
                    kind: FieldKind::String,
                    options: Vec::new(),
                    default: Some("free".to_string()),
                },
                SettingField {
                    key: format!("plugin.{id}.nodefault"),
                    label: "No Default".to_string(),
                    help: String::new(),
                    secret: false,
                    required: false,
                    kind: FieldKind::String,
                    options: Vec::new(),
                    default: None,
                },
            ],
            mcp: vec![],
            skills: vec![],
            provider: None,
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

    #[tokio::test]
    async fn get_falls_back_to_a_registered_plugin_fields_default_when_unset() {
        // Unique id — `plugin_field` is a process-wide registry, so a shared
        // id could pick up state from another test's plugin in the same
        // test binary.
        let id = "task-c3-storetest-default-plugin";
        register_plugin_with_default(id, false);
        let key = format!("plugin.{id}.tier");
        let no_default_key = format!("plugin.{id}.nodefault");

        let (store, _tmp) = open_test_store().await;
        let settings = SettingsStore::new(std::sync::Arc::new(store));

        // Unset: falls back to the plugin field's declared default — this
        // is the fix (previously `get` only consulted the static
        // `find_field` catalog, which never covers `plugin.*` keys, so a
        // manifest `default` had no effect on the read path at all).
        assert_eq!(settings.get(&key).await.unwrap().as_deref(), Some("free"));
        // A plugin field with no `default` still resolves to `None` when unset.
        assert_eq!(settings.get(&no_default_key).await.unwrap(), None);

        // An explicit stored value takes precedence over the plugin default.
        settings.set(&key, "pro").await.unwrap();
        assert_eq!(settings.get(&key).await.unwrap().as_deref(), Some("pro"));
    }

    #[tokio::test]
    async fn missing_required_treats_a_required_plugin_field_with_a_default_as_satisfied() {
        let id = "task-c3-storetest-required-default-plugin";
        register_plugin_with_default(id, true);
        let key = format!("plugin.{id}.tier");

        let (store, _tmp) = open_test_store().await;
        let settings = SettingsStore::new(std::sync::Arc::new(store));
        settings
            .set(&format!("plugin.{id}.enabled"), "true")
            .await
            .unwrap();

        // Required, never explicitly set, but the field declares a
        // `default` — `missing_required` calls the same `get()` used for
        // substitution, so the default resolves it and it must not be
        // reported as missing. This mirrors pre-existing behavior for
        // static/global required fields with a schema default; it was
        // simply unreachable for plugin fields before this fix.
        assert!(!settings
            .missing_required()
            .await
            .unwrap()
            .iter()
            .any(|k| k == &key));
    }

    #[test]
    fn csv_trims_and_drops_empties() {
        assert_eq!(csv(Some("a, b,,c ")), vec!["a", "b", "c"]);
        assert!(csv(None).is_empty());
        assert!(csv(Some("")).is_empty());
    }

    #[test]
    fn models_meta_keys_validate_as_json_objects() {
        assert_eq!(
            validate_setting(
                "models.meta.claude-sonnet-4-5",
                r#"{"context_window":200000}"#
            ),
            None
        );
        assert_eq!(
            validate_setting("models.meta.x", "not json"),
            Some("models.meta.x must be a JSON object".to_string())
        );
        assert_eq!(
            validate_setting("models.meta.", "{}"),
            Some("unknown setting: models.meta.".to_string())
        );
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
            .iter()
            .any(|k| k == "workdir_root"));
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
