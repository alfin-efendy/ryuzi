//! First-party plugins that don't have a more natural home beside their own
//! implementation module. `native` lives beside its harness code
//! (`harness::native::native_plugin`); `discord` — the only built-in gateway
//! today — lives here since `gateway::discord` is data/protocol-only.

use ryuzi_plugin_sdk::{FieldKind, PluginManifest, SettingField};

use crate::gateway::discord;
use crate::settings::{ConfigField, FieldType};

use super::host::{CorePlugin, PluginSource};

/// Map the static `ConfigField` catalog (the settings-schema source of
/// truth — see `settings::catalog`) into the SDK's `SettingField` shape, so
/// a built-in's manifest never hand-duplicates field definitions that the
/// settings schema already owns.
fn fields_to_sdk(fields: &[ConfigField]) -> Vec<SettingField> {
    fields
        .iter()
        .map(|f| SettingField {
            key: f.key.to_string(),
            label: f.label.to_string(),
            help: f.help.to_string(),
            secret: f.secret,
            required: f.required,
            kind: match f.field_type {
                FieldType::Int => FieldKind::Int,
                FieldType::String | FieldType::Enum => FieldKind::String,
            },
            options: f.one_of.iter().map(|s| s.to_string()).collect(),
            default: f.default.map(str::to_string),
        })
        .collect()
}

/// The `discord` built-in: a gateway-only plugin. Its `GatewayFactory` is the
/// same one `crate::daemon::build_daemon` has always wired via
/// `gateway::discord::factory_entries()` — empty under `not(feature =
/// "discord")`, populated with the real `serenity`-backed factory when the
/// feature is on (see that function's doc for why the feature gate lives in
/// `ryuzi-core` rather than `ryuzi-runner`).
pub fn discord_plugin() -> CorePlugin {
    let gateway = discord::factory_entries()
        .into_iter()
        .find(|(id, _)| id == "discord")
        .map(|(_, factory)| factory);

    CorePlugin {
        manifest: PluginManifest {
            contract: 1,
            id: "discord".to_string(),
            name: "Discord".to_string(),
            version: "0.0.0".to_string(),
            publisher: "ryuzi".to_string(),
            description: "Drive sessions from a Discord server".to_string(),
            homepage: None,
            icon: Some("message-circle".to_string()),
            categories: vec!["chat-gateway".to_string()],
            slot: None,
            verified: true,
            experimental: false,
            auth: None,
            settings: fields_to_sdk(crate::settings::catalog::DISCORD_FIELDS),
            mcp: vec![],
            extensions: vec![],
            skills: vec![],
            provider: None,
        },
        harness: None,
        gateway,
        connector: None,
        extension: None,
        source: PluginSource::Builtin,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discord_plugin_manifest_has_expected_identity() {
        let plugin = discord_plugin();
        assert_eq!(plugin.manifest.contract, 1);
        assert_eq!(plugin.manifest.id, "discord");
        assert_eq!(plugin.manifest.name, "Discord");
        assert_eq!(plugin.manifest.publisher, "ryuzi");
        assert!(plugin.manifest.verified);
        assert_eq!(plugin.manifest.categories, vec!["chat-gateway".to_string()]);
        assert_eq!(plugin.manifest.icon.as_deref(), Some("message-circle"));
        assert!(plugin.harness.is_none());
        assert!(plugin.connector.is_none());
    }

    #[test]
    fn discord_plugin_settings_mirror_discord_fields_keys() {
        let plugin = discord_plugin();
        let keys: Vec<&str> = plugin
            .manifest
            .settings
            .iter()
            .map(|f| f.key.as_str())
            .collect();
        assert_eq!(
            keys,
            vec!["discord.token", "discord.app_id", "discord.guild_id"]
        );
        // secret/required flags carried over from the static catalog.
        assert!(plugin.manifest.settings[0].secret);
        assert!(plugin.manifest.settings.iter().all(|f| f.required));
    }

    #[cfg(feature = "discord")]
    #[test]
    fn discord_plugin_has_a_gateway_factory_when_the_feature_is_on() {
        assert!(discord_plugin().gateway.is_some());
    }

    #[cfg(not(feature = "discord"))]
    #[test]
    fn discord_plugin_has_no_gateway_factory_without_the_feature() {
        assert!(discord_plugin().gateway.is_none());
    }
}
