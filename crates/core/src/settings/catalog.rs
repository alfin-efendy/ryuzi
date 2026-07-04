//! Data-only provider catalog: gateway/runtime descriptors and their
//! provider-specific `ConfigField`s. Field keys are user-visible contracts —
//! settings stored under these keys must keep resolving across releases.

use crate::settings::fields::{ConfigField, BASE, GLOBAL_FIELDS};

pub struct GatewayDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub fields: &'static [ConfigField],
}

pub struct RuntimeDescriptor {
    pub id: &'static str,
    pub label: &'static str,
    pub description: &'static str,
    pub fields: &'static [ConfigField],
}

pub struct ProviderCatalog {
    pub gateways: &'static [GatewayDescriptor],
    pub runtimes: &'static [RuntimeDescriptor],
}

impl ProviderCatalog {
    pub fn gateway(&self, id: &str) -> Option<&'static GatewayDescriptor> {
        self.gateways.iter().find(|g| g.id == id)
    }

    pub fn runtime(&self, id: &str) -> Option<&'static RuntimeDescriptor> {
        self.runtimes.iter().find(|r| r.id == id)
    }
}

pub static DISCORD_FIELDS: &[ConfigField] = &[
    ConfigField {
        key: "discord.token",
        label: "Bot token",
        secret: true,
        required: true,
        help: "Discord Developer Portal -> your app -> Bot -> Reset Token",
        example: Some("MTk4Nj...long.secret"),
        ..BASE
    },
    ConfigField {
        key: "discord.app_id",
        label: "Application ID",
        required: true,
        help: "Developer Portal -> General Information -> Application ID",
        example: Some("123456789012345678"),
        ..BASE
    },
    ConfigField {
        key: "discord.guild_id",
        label: "Server (guild) ID",
        required: true,
        help: "Enable Developer Mode, right-click your server -> Copy Server ID",
        example: Some("987654321098765432"),
        ..BASE
    },
];

pub static CATALOG: ProviderCatalog = ProviderCatalog {
    gateways: &[GatewayDescriptor {
        id: "discord",
        label: "Discord",
        description: "Drive sessions from a Discord server",
        fields: DISCORD_FIELDS,
    }],
    runtimes: &[RuntimeDescriptor {
        id: "claude-code",
        label: "Claude Code",
        description: "Anthropic's Claude Code CLI (uses your host login)",
        fields: &[],
    }],
};

/// All fields in schema order: globals, then each gateway's fields, then
/// each runtime's fields.
pub fn all_fields() -> Vec<&'static ConfigField> {
    GLOBAL_FIELDS
        .iter()
        .chain(CATALOG.gateways.iter().flat_map(|g| g.fields.iter()))
        .chain(CATALOG.runtimes.iter().flat_map(|r| r.fields.iter()))
        .collect()
}

pub fn find_field(key: &str) -> Option<&'static ConfigField> {
    all_fields().into_iter().find(|f| f.key == key)
}

pub fn is_secret(key: &str) -> bool {
    find_field(key).map(|f| f.secret).unwrap_or(false)
}
