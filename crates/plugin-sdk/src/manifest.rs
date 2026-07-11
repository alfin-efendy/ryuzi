//! The plugin manifest: the declarative contract every Ryuzi plugin
//! (built-in, embedded catalog, or user-authored) satisfies. This module
//! owns parsing (TOML) and structural validation only — it has no opinion
//! on how a manifest becomes a running harness, gateway, or connector; that
//! binding lives in `ryuzi-core`'s `PluginHost`.

use std::collections::{BTreeMap, HashSet};

use serde::{Deserialize, Serialize};

use crate::categories;

/// The manifest contract version this SDK understands. `validate()` rejects
/// manifests declaring a newer `contract` so an old loader fails loudly
/// instead of silently misinterpreting fields it doesn't know about.
pub const CONTRACT_VERSION: u32 = 1;

/// One plugin, one manifest. Rust built-ins construct this in code; catalog
/// and user plugins author it as TOML (`ryuzi-plugin.toml`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginManifest {
    pub contract: u32,
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub publisher: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub homepage: Option<String>,
    #[serde(default)]
    pub icon: Option<String>,
    #[serde(default)]
    pub categories: Vec<String>,
    /// Optional exclusive capability claim: "I am THE provider of the
    /// `<slot>` capability" (e.g. `slot = "memory"` for a Hermes memory
    /// backend). Distinct from `categories` — a category is a free-form
    /// cosmetic tag any number of plugins may share; a slot is arbitrated
    /// by `ryuzi_core`'s `PluginHost` (first registration wins, see
    /// `PluginHost::slot_owner`), and a losing claim is surfaced as a
    /// `plugin_doctor` `"slot-conflict"` finding rather than silently
    /// dropped. An unknown slot name is a non-fatal `warnings()` entry,
    /// matching `categories`' warn-not-reject discipline — it is never a
    /// `validate()` error.
    #[serde(default)]
    pub slot: Option<String>,
    #[serde(default)]
    pub verified: bool,
    #[serde(default)]
    pub experimental: bool,
    #[serde(default)]
    pub auth: Option<AuthSpec>,
    #[serde(default)]
    pub settings: Vec<SettingField>,
    #[serde(default)]
    pub mcp: Vec<McpServerDef>,
    /// Supervised subprocess "code plugin" extensions (Track D). The TOML
    /// table is the singular `[[extension]]` (matching the design doc and
    /// the `[[mcp]]`/`[auth]` singular-noun convention), hence the
    /// `rename`; the Rust field stays plural per `Vec` naming convention.
    /// Extension names are validated for uniqueness in their own
    /// namespace, deliberately separate from `mcp` server names: an
    /// extension and an MCP server are different capability axes wired
    /// independently by `ryuzi-core`'s `PluginHost`, so a name collision
    /// between them is not ambiguous and not rejected (mirrors how
    /// `settings` keys already live in a namespace separate from `mcp`
    /// names).
    #[serde(default, rename = "extension")]
    pub extensions: Vec<ExtensionDef>,
    #[serde(default)]
    pub skills: Vec<SkillDef>,
    #[serde(default)]
    pub provider: Option<ProviderMeta>,
}

/// How a plugin authenticates. `none` needs no credential; `api-key` and
/// `token` read a secret (via `setting` and/or `env` fallback); `oauth`
/// delegates to provider-specific machinery elsewhere (e.g. `llm_router`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum AuthKind {
    #[default]
    None,
    ApiKey,
    Token,
    Oauth,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
pub struct AuthSpec {
    pub kind: AuthKind,
    pub setting: Option<String>,
    pub env: Option<String>,
    #[serde(alias = "help_url")]
    pub help_url: Option<String>,
    pub authorize_url: Option<String>,
    pub token_url: Option<String>,
    pub resource: Option<String>,
    pub scopes: Vec<String>,
    pub client_id_setting: Option<String>,
    pub client_secret_setting: Option<String>,
    pub dynamic_registration: bool,
    pub extra_authorize_params: BTreeMap<String, String>,
    pub extra_token_params: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SettingField {
    pub key: String,
    pub label: String,
    #[serde(default)]
    pub help: String,
    #[serde(default)]
    pub secret: bool,
    #[serde(default)]
    pub required: bool,
    #[serde(default)]
    pub kind: FieldKind,
    /// When non-empty, this field is an enum/choice: the persisted value
    /// must be one of these members (enforced by
    /// `ryuzi_core::settings::store::validate_plugin_field`). Expressed as
    /// `kind = "string"` + non-empty `options` — `validate()` rejects any
    /// other `kind` paired with non-empty `options`.
    #[serde(default)]
    pub options: Vec<String>,
    /// Pre-filled/effective value to show when no row is persisted yet. When
    /// `options` is non-empty, `default` (if set) must be one of its
    /// members.
    #[serde(default)]
    pub default: Option<String>,
}

/// The value shape a `SettingField` renders and stores. Defaults to
/// `String` since most settings (tokens, hostnames, ids) are plain text.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FieldKind {
    #[default]
    String,
    Int,
    Bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerDef {
    pub name: String,
    pub transport: McpTransportDef,
    #[serde(default)]
    pub command: Option<String>,
    #[serde(default)]
    pub args: Vec<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub headers: BTreeMap<String, String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum McpTransportDef {
    Stdio,
    Http,
}

/// The hook events an `[[extension]]` may name in its `events[]` list —
/// the SDK's own copy of Track C's hook-event vocabulary. The SDK cannot
/// depend on `ryuzi-core` (see module docs), so this is a hand-kept
/// duplicate of `ryuzi_core::harness::native::hooks::HookEvent::as_str()`.
/// **Keep these two lists in sync**: if `HookEvent` gains, renames, or
/// removes a variant, update this constant in the same change, or
/// extension manifests will wrongly reject a real event (or silently
/// accept a dead one that never fires).
pub const KNOWN_HOOK_EVENTS: &[&str] =
    &["session.start", "tool.before", "tool.after", "session.end"];

/// Ceiling for `ExtensionDef::timeout_ms`. A gating extension (subscribed
/// to `tool.before`) blocks the agent for up to this long before the host's
/// fail-open policy kicks in (Track D runtime, not this slice); anything
/// larger is almost certainly a manifest typo, not an intentional budget.
/// Keep in sync with the number in `ManifestError::ExtensionTimeoutOutOfRange`'s
/// message.
pub const MAX_EXTENSION_TIMEOUT_MS: u64 = 60_000;

/// One supervised subprocess "code plugin" extension (Track D). Declarative
/// only in this slice — DT1 adds manifest parsing and validation; the
/// `ExtensionHost` that actually spawns, supervises, and dispatches
/// `events[]` to this command is a later Track D slice.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtensionDef {
    /// Unique within the manifest's `extensions` list (see the field doc on
    /// `PluginManifest::extensions` for why this namespace is separate from
    /// `mcp` server names).
    pub name: String,
    /// The stdio binary to spawn, or a `${...}` placeholder (`${auth}`,
    /// `${setting:KEY}`) resolved the same way `McpServerDef::command` is.
    /// Required and must be non-empty.
    pub command: String,
    #[serde(default)]
    pub args: Vec<String>,
    /// Hook events this extension subscribes to. Every entry must be a
    /// member of [`KNOWN_HOOK_EVENTS`] — unlike unknown `categories`/`slot`
    /// values (which only warn), an unknown event is a hard `validate()`
    /// error, because a typo'd event silently never fires rather than
    /// merely showing an odd label.
    #[serde(default)]
    pub events: Vec<String>,
    /// If true, the host queries this extension for tool definitions at
    /// init and wires them into the session's tool registry.
    #[serde(default)]
    pub provides_tools: bool,
    /// Per-event response budget in milliseconds. When present, must be
    /// `> 0` and `<= MAX_EXTENSION_TIMEOUT_MS`.
    #[serde(default)]
    pub timeout_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelDef {
    pub id: String,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub default: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderMeta {
    pub format: String,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub models: Vec<ModelDef>,
}

/// Errors from parsing or validating a `PluginManifest`.
#[derive(Debug, thiserror::Error)]
pub enum ManifestError {
    #[error("invalid plugin manifest toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error(
        "manifest declares contract {found}, but this build only supports up to {CONTRACT_VERSION}"
    )]
    ContractTooNew { found: u32 },
    #[error("invalid plugin id: {0}")]
    InvalidId(String),
    #[error("plugin name must not be empty")]
    EmptyName,
    #[error("duplicate mcp server name: {0}")]
    DuplicateMcpName(String),
    #[error("mcp server \"{0}\" uses stdio transport but has no command")]
    MissingCommand(String),
    #[error("mcp server \"{0}\" uses http transport but has no url")]
    MissingUrl(String),
    #[error("mcp server \"{0}\" references ${{auth}} but the manifest has no [auth] block")]
    AuthPlaceholderWithoutAuth(String),
    #[error("duplicate settings field key: {0}")]
    DuplicateSettingKey(String),
    #[error("settings field \"{0}\" declares non-empty `options` but `kind` is not `string`")]
    SettingOptionsRequireStringKind(String),
    #[error("settings field \"{0}\"'s `default` is not a member of its `options`")]
    SettingDefaultNotInOptions(String),
    #[error("duplicate extension name: {0}")]
    DuplicateExtensionName(String),
    #[error("extension \"{0}\" has an empty command")]
    ExtensionEmptyCommand(String),
    #[error("extension \"{0}\" subscribes to unknown hook event \"{1}\"")]
    ExtensionUnknownEvent(String, String),
    #[error("extension \"{0}\"'s timeout_ms must be > 0 and <= 60000 (got {1})")]
    ExtensionTimeoutOutOfRange(String, u64),
    #[error("extension \"{0}\" references ${{auth}} but the manifest has no [auth] block")]
    ExtensionAuthPlaceholderWithoutAuth(String),
}

fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

fn contains_auth_placeholder(server: &McpServerDef) -> bool {
    const PLACEHOLDER: &str = "${auth}";
    server.env.values().any(|v| v.contains(PLACEHOLDER))
        || server.headers.values().any(|v| v.contains(PLACEHOLDER))
        || server.args.iter().any(|a| a.contains(PLACEHOLDER))
        || server
            .url
            .as_deref()
            .is_some_and(|u| u.contains(PLACEHOLDER))
}

fn extension_contains_auth_placeholder(extension: &ExtensionDef) -> bool {
    const PLACEHOLDER: &str = "${auth}";
    extension.command.contains(PLACEHOLDER)
        || extension.args.iter().any(|a| a.contains(PLACEHOLDER))
}

impl PluginManifest {
    /// Parse TOML into a manifest and validate it in one step.
    pub fn from_toml(input: &str) -> Result<PluginManifest, ManifestError> {
        let manifest: PluginManifest = toml::from_str(input)?;
        manifest.validate()?;
        Ok(manifest)
    }

    /// Structural validation: contract version, id shape, required fields,
    /// unique MCP server names, transport-specific requirements, and the
    /// `${auth}` placeholder requiring an `[auth]` block.
    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.contract > CONTRACT_VERSION {
            return Err(ManifestError::ContractTooNew {
                found: self.contract,
            });
        }
        if !is_valid_id(&self.id) {
            return Err(ManifestError::InvalidId(self.id.clone()));
        }
        if self.name.is_empty() {
            return Err(ManifestError::EmptyName);
        }

        let mut seen_setting_keys: HashSet<&str> = HashSet::new();
        for field in &self.settings {
            if !seen_setting_keys.insert(field.key.as_str()) {
                return Err(ManifestError::DuplicateSettingKey(field.key.clone()));
            }
            if !field.options.is_empty() {
                if field.kind != FieldKind::String {
                    return Err(ManifestError::SettingOptionsRequireStringKind(
                        field.key.clone(),
                    ));
                }
                if let Some(default) = &field.default {
                    if !field.options.iter().any(|o| o == default) {
                        return Err(ManifestError::SettingDefaultNotInOptions(field.key.clone()));
                    }
                }
            }
        }

        let mut seen_mcp_names: HashSet<&str> = HashSet::new();
        for server in &self.mcp {
            if !seen_mcp_names.insert(server.name.as_str()) {
                return Err(ManifestError::DuplicateMcpName(server.name.clone()));
            }
            match server.transport {
                McpTransportDef::Stdio if server.command.is_none() => {
                    return Err(ManifestError::MissingCommand(server.name.clone()));
                }
                McpTransportDef::Http if server.url.is_none() => {
                    return Err(ManifestError::MissingUrl(server.name.clone()));
                }
                McpTransportDef::Stdio | McpTransportDef::Http => {}
            }
            if contains_auth_placeholder(server) && self.auth.is_none() {
                return Err(ManifestError::AuthPlaceholderWithoutAuth(
                    server.name.clone(),
                ));
            }
        }

        let mut seen_extension_names: HashSet<&str> = HashSet::new();
        for extension in &self.extensions {
            if !seen_extension_names.insert(extension.name.as_str()) {
                return Err(ManifestError::DuplicateExtensionName(
                    extension.name.clone(),
                ));
            }
            if extension.command.trim().is_empty() {
                return Err(ManifestError::ExtensionEmptyCommand(extension.name.clone()));
            }
            for event in &extension.events {
                if !KNOWN_HOOK_EVENTS.contains(&event.as_str()) {
                    return Err(ManifestError::ExtensionUnknownEvent(
                        extension.name.clone(),
                        event.clone(),
                    ));
                }
            }
            if let Some(timeout_ms) = extension.timeout_ms {
                if timeout_ms == 0 || timeout_ms > MAX_EXTENSION_TIMEOUT_MS {
                    return Err(ManifestError::ExtensionTimeoutOutOfRange(
                        extension.name.clone(),
                        timeout_ms,
                    ));
                }
            }
            if extension_contains_auth_placeholder(extension) && self.auth.is_none() {
                return Err(ManifestError::ExtensionAuthPlaceholderWithoutAuth(
                    extension.name.clone(),
                ));
            }
        }

        Ok(())
    }

    /// Non-fatal feedback: categories outside the standard vocabulary
    /// (`categories::KNOWN`), plus a claimed `slot` outside
    /// `categories::KNOWN_SLOTS` (if any). Unlike `validate()`, this never
    /// rejects a manifest — new categories/slots should not break the
    /// loader.
    pub fn warnings(&self) -> Vec<String> {
        let mut warnings: Vec<String> = self
            .categories
            .iter()
            .filter(|category| !categories::KNOWN.contains(&category.as_str()))
            .cloned()
            .collect();
        if let Some(slot) = &self.slot {
            if !categories::KNOWN_SLOTS.contains(&slot.as_str()) {
                warnings.push(slot.clone());
            }
        }
        warnings
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const GITHUB_MANIFEST: &str = r#"
contract = 1
id = "github"
name = "GitHub"
version = "0.1.0"
publisher = "ryuzi"
description = "Repos, issues, PRs, and wiki via the GitHub MCP server."
homepage = "https://github.com"
icon = "github"
categories = ["vcs", "issues"]
verified = true

[auth]
kind = "token"
setting = "plugin.github.token"
env = "GITHUB_PERSONAL_ACCESS_TOKEN"
help_url = "https://github.com/settings/tokens"

[[settings]]
key = "plugin.github.host"
label = "GitHub host"
help = "Set for GitHub Enterprise."
required = false

[[mcp]]
name = "github"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_PERSONAL_ACCESS_TOKEN = "${auth}" }

[[skills]]
name = "github-triage"
description = "Triage issues into labeled buckets"
path = "skills/github-triage"
"#;

    #[test]
    fn round_trips_the_github_example_manifest() {
        let manifest =
            PluginManifest::from_toml(GITHUB_MANIFEST).expect("should parse and validate");

        assert_eq!(manifest.contract, 1);
        assert_eq!(manifest.id, "github");
        assert_eq!(manifest.name, "GitHub");
        assert_eq!(manifest.version, "0.1.0");
        assert_eq!(manifest.publisher, "ryuzi");
        assert_eq!(manifest.homepage.as_deref(), Some("https://github.com"));
        assert_eq!(manifest.icon.as_deref(), Some("github"));
        assert_eq!(manifest.categories, vec!["vcs", "issues"]);
        assert!(manifest.verified);
        assert!(!manifest.experimental);

        let auth = manifest.auth.expect("auth block");
        assert_eq!(auth.kind, AuthKind::Token);
        assert_eq!(auth.setting.as_deref(), Some("plugin.github.token"));
        assert_eq!(auth.env.as_deref(), Some("GITHUB_PERSONAL_ACCESS_TOKEN"));
        assert_eq!(
            auth.help_url.as_deref(),
            Some("https://github.com/settings/tokens")
        );

        assert_eq!(manifest.settings.len(), 1);
        let setting = &manifest.settings[0];
        assert_eq!(setting.key, "plugin.github.host");
        assert_eq!(setting.label, "GitHub host");
        assert_eq!(setting.help, "Set for GitHub Enterprise.");
        assert!(!setting.required);
        assert!(!setting.secret);
        assert_eq!(setting.kind, FieldKind::String);
        assert!(setting.options.is_empty());
        assert_eq!(setting.default, None);

        assert_eq!(manifest.mcp.len(), 1);
        let mcp = &manifest.mcp[0];
        assert_eq!(mcp.name, "github");
        assert_eq!(mcp.transport, McpTransportDef::Stdio);
        assert_eq!(mcp.command.as_deref(), Some("npx"));
        assert_eq!(
            mcp.args,
            vec![
                "-y".to_string(),
                "@modelcontextprotocol/server-github".to_string()
            ]
        );
        assert_eq!(
            mcp.env
                .get("GITHUB_PERSONAL_ACCESS_TOKEN")
                .map(String::as_str),
            Some("${auth}")
        );

        assert_eq!(manifest.skills.len(), 1);
        let skill = &manifest.skills[0];
        assert_eq!(skill.name, "github-triage");
        assert_eq!(skill.description, "Triage issues into labeled buckets");
        assert_eq!(skill.path, "skills/github-triage");
    }

    #[test]
    fn round_trips_the_provider_block() {
        let toml_str = r#"
contract = 1
id = "anthropic"
name = "Anthropic"

[provider]
format = "anthropic"
base_url = "https://api.anthropic.com"
models = [ { id = "claude-opus-4-5", label = "Opus 4.5", default = true } ]
"#;
        let manifest = PluginManifest::from_toml(toml_str).expect("should parse and validate");

        let provider = manifest.provider.expect("provider block");
        assert_eq!(provider.format, "anthropic");
        assert_eq!(
            provider.base_url.as_deref(),
            Some("https://api.anthropic.com")
        );
        assert_eq!(provider.models.len(), 1);
        assert_eq!(provider.models[0].id, "claude-opus-4-5");
        assert_eq!(provider.models[0].label.as_deref(), Some("Opus 4.5"));
        assert!(provider.models[0].default);
    }

    #[test]
    fn parses_oauth_auth_metadata() {
        let toml_str = r#"
contract = 1
id = "acme-oauth"
name = "Acme OAuth"

[auth]
kind = "oauth"
setting = "plugin.acme.oauth_setting"
env = "ACME_OAUTH"
help_url = "https://acme.example.com/help"
authorize-url = "https://acme.example.com/oauth/authorize"
token-url = "https://acme.example.com/oauth/token"
resource = "acme://api"
scopes = ["repo", "issues:read"]
client-id-setting = "plugin.acme.client_id"
client-secret-setting = "plugin.acme.client_secret"
dynamic-registration = true
extra-authorize-params = { prompt = "consent", access_type = "offline" }
extra-token-params = { audience = "acme", tenant = "engineering" }
"#;
        let manifest = PluginManifest::from_toml(toml_str).expect("should parse and validate");

        let auth = manifest.auth.expect("auth block");
        assert_eq!(auth.kind, AuthKind::Oauth);
        assert_eq!(auth.setting.as_deref(), Some("plugin.acme.oauth_setting"));
        assert_eq!(auth.env.as_deref(), Some("ACME_OAUTH"));
        assert_eq!(
            auth.help_url.as_deref(),
            Some("https://acme.example.com/help")
        );
        assert_eq!(
            auth.authorize_url.as_deref(),
            Some("https://acme.example.com/oauth/authorize")
        );
        assert_eq!(
            auth.token_url.as_deref(),
            Some("https://acme.example.com/oauth/token")
        );
        assert_eq!(auth.resource.as_deref(), Some("acme://api"));
        assert_eq!(
            auth.scopes,
            vec!["repo".to_string(), "issues:read".to_string()]
        );
        assert_eq!(
            auth.client_id_setting.as_deref(),
            Some("plugin.acme.client_id")
        );
        assert_eq!(
            auth.client_secret_setting.as_deref(),
            Some("plugin.acme.client_secret")
        );
        assert!(auth.dynamic_registration);
        assert_eq!(
            auth.extra_authorize_params
                .get("prompt")
                .map(String::as_str),
            Some("consent")
        );
        assert_eq!(
            auth.extra_authorize_params
                .get("access_type")
                .map(String::as_str),
            Some("offline")
        );
        assert_eq!(
            auth.extra_token_params.get("audience").map(String::as_str),
            Some("acme")
        );
        assert_eq!(
            auth.extra_token_params.get("tenant").map(String::as_str),
            Some("engineering")
        );
    }

    #[test]
    fn parses_canonical_help_url_key() {
        let toml_str = r#"
contract = 1
id = "acme-oauth-help-url"
name = "Acme OAuth Help URL"

[auth]
kind = "oauth"
help-url = "https://acme.example.com/help"
"#;
        let manifest = PluginManifest::from_toml(toml_str).expect("should parse and validate");
        let auth = manifest.auth.expect("auth block");
        assert_eq!(
            auth.help_url.as_deref(),
            Some("https://acme.example.com/help")
        );
    }

    #[test]
    fn parses_oauth_with_only_kind_for_backwards_compatibility() {
        let toml_str = r#"
contract = 1
id = "acme-oauth-legacy"
name = "Acme OAuth Legacy"

[auth]
kind = "oauth"
"#;
        let manifest = PluginManifest::from_toml(toml_str).expect("should parse and validate");

        let auth = manifest.auth.expect("auth block");
        assert_eq!(auth.kind, AuthKind::Oauth);
        assert_eq!(auth.setting, None);
        assert_eq!(auth.env, None);
        assert_eq!(auth.help_url, None);
        assert_eq!(auth.authorize_url, None);
        assert_eq!(auth.token_url, None);
        assert_eq!(auth.resource, None);
        assert_eq!(auth.scopes, Vec::<String>::new());
        assert_eq!(auth.client_id_setting, None);
        assert_eq!(auth.client_secret_setting, None);
        assert!(!auth.dynamic_registration);
        assert_eq!(auth.extra_authorize_params, BTreeMap::new());
        assert_eq!(auth.extra_token_params, BTreeMap::new());
    }

    fn minimal_manifest(extra: &str) -> String {
        format!(
            r#"
contract = 1
id = "acme"
name = "Acme"
{extra}
"#
        )
    }

    #[test]
    fn rejects_missing_id() {
        let toml_str = r#"
contract = 1
name = "Acme"
"#;
        let err = PluginManifest::from_toml(toml_str).expect_err("missing id should fail to parse");
        assert!(matches!(err, ManifestError::Toml(_)));
    }

    #[test]
    fn rejects_uppercase_id() {
        let toml_str = r#"
contract = 1
id = "Acme"
name = "Acme"
"#;
        let err =
            PluginManifest::from_toml(toml_str).expect_err("uppercase id should fail validation");
        assert!(matches!(err, ManifestError::InvalidId(id) if id == "Acme"));
    }

    #[test]
    fn rejects_contract_newer_than_supported() {
        let toml_str = r#"
contract = 2
id = "acme"
name = "Acme"
"#;
        let err =
            PluginManifest::from_toml(toml_str).expect_err("contract 2 should fail validation");
        assert!(matches!(err, ManifestError::ContractTooNew { found: 2 }));
    }

    #[test]
    fn rejects_duplicate_mcp_names() {
        let toml_str = minimal_manifest(
            r#"
[[mcp]]
name = "dup"
transport = "stdio"
command = "npx"

[[mcp]]
name = "dup"
transport = "stdio"
command = "npx"
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("duplicate mcp names should fail validation");
        assert!(matches!(err, ManifestError::DuplicateMcpName(name) if name == "dup"));
    }

    #[test]
    fn rejects_auth_placeholder_without_auth_block() {
        let toml_str = minimal_manifest(
            r#"
[[mcp]]
name = "svc"
transport = "stdio"
command = "npx"
env = { TOKEN = "${auth}" }
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("${auth} without [auth] should fail validation");
        assert!(matches!(err, ManifestError::AuthPlaceholderWithoutAuth(name) if name == "svc"));
    }

    #[test]
    fn stdio_transport_requires_command() {
        let toml_str = minimal_manifest(
            r#"
[[mcp]]
name = "svc"
transport = "stdio"
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("stdio without command should fail validation");
        assert!(matches!(err, ManifestError::MissingCommand(name) if name == "svc"));
    }

    #[test]
    fn http_transport_requires_url() {
        let toml_str = minimal_manifest(
            r#"
[[mcp]]
name = "svc"
transport = "http"
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("http without url should fail validation");
        assert!(matches!(err, ManifestError::MissingUrl(name) if name == "svc"));
    }

    #[test]
    fn unknown_category_is_a_warning_not_an_error() {
        let toml_str = minimal_manifest(r#"categories = ["not-a-real-category"]"#);
        let manifest =
            PluginManifest::from_toml(&toml_str).expect("unknown category should still parse");
        assert_eq!(manifest.warnings(), vec!["not-a-real-category".to_string()]);
    }

    #[test]
    fn known_categories_produce_no_warnings() {
        let toml_str = minimal_manifest(r#"categories = ["vcs", "issues"]"#);
        let manifest = PluginManifest::from_toml(&toml_str).expect("known categories should parse");
        assert!(manifest.warnings().is_empty());
    }

    // ---------- SettingField: options/default (Feature C3) ----------

    #[test]
    fn settings_field_with_valid_enum_options_and_default_parses() {
        let toml_str = minimal_manifest(
            r#"
[[settings]]
key = "plugin.acme.tier"
label = "Tier"
kind = "string"
options = ["free", "pro", "enterprise"]
default = "free"
"#,
        );
        let manifest =
            PluginManifest::from_toml(&toml_str).expect("valid enum settings field should parse");
        let field = &manifest.settings[0];
        assert_eq!(field.kind, FieldKind::String);
        assert_eq!(
            field.options,
            vec![
                "free".to_string(),
                "pro".to_string(),
                "enterprise".to_string()
            ]
        );
        assert_eq!(field.default.as_deref(), Some("free"));
    }

    #[test]
    fn rejects_duplicate_setting_keys() {
        let toml_str = minimal_manifest(
            r#"
[[settings]]
key = "plugin.acme.dup"
label = "Dup One"

[[settings]]
key = "plugin.acme.dup"
label = "Dup Two"
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("duplicate settings field keys should fail validation");
        assert!(matches!(err, ManifestError::DuplicateSettingKey(key) if key == "plugin.acme.dup"));
    }

    #[test]
    fn rejects_default_not_a_member_of_options() {
        let toml_str = minimal_manifest(
            r#"
[[settings]]
key = "plugin.acme.tier"
label = "Tier"
kind = "string"
options = ["free", "pro"]
default = "enterprise"
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("default outside options should fail validation");
        assert!(
            matches!(err, ManifestError::SettingDefaultNotInOptions(key) if key == "plugin.acme.tier")
        );
    }

    #[test]
    fn rejects_options_paired_with_non_string_kind() {
        let toml_str = minimal_manifest(
            r#"
[[settings]]
key = "plugin.acme.retries"
label = "Retries"
kind = "int"
options = ["1", "2", "3"]
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("options with a non-string kind should fail validation");
        assert!(
            matches!(err, ManifestError::SettingOptionsRequireStringKind(key) if key == "plugin.acme.retries")
        );
    }

    // ---------- slot (Feature C2) ----------

    #[test]
    fn slot_defaults_to_none_when_omitted() {
        let toml_str = minimal_manifest("");
        let manifest = PluginManifest::from_toml(&toml_str).expect("should parse");
        assert_eq!(manifest.slot, None);
        assert!(manifest.warnings().is_empty());
    }

    #[test]
    fn known_slot_parses_with_no_warning() {
        let toml_str = minimal_manifest(r#"slot = "memory""#);
        let manifest =
            PluginManifest::from_toml(&toml_str).expect("known slot should parse and validate");
        assert_eq!(manifest.slot.as_deref(), Some("memory"));
        assert!(manifest.warnings().is_empty());
    }

    #[test]
    fn unknown_slot_is_a_warning_not_an_error() {
        let toml_str = minimal_manifest(r#"slot = "not-a-real-slot""#);
        let manifest =
            PluginManifest::from_toml(&toml_str).expect("unknown slot should still parse");
        assert_eq!(manifest.slot.as_deref(), Some("not-a-real-slot"));
        assert_eq!(manifest.warnings(), vec!["not-a-real-slot".to_string()]);
    }

    #[test]
    fn unknown_category_and_unknown_slot_both_surface_as_warnings() {
        let toml_str = minimal_manifest(
            r#"
categories = ["not-a-real-category"]
slot = "not-a-real-slot"
"#,
        );
        let manifest =
            PluginManifest::from_toml(&toml_str).expect("unknown category+slot should still parse");
        assert_eq!(
            manifest.warnings(),
            vec![
                "not-a-real-category".to_string(),
                "not-a-real-slot".to_string()
            ]
        );
    }

    // ---------- extension (Track D, Slice DT1) ----------

    #[test]
    fn parses_a_valid_extension_declaration() {
        let toml_str = minimal_manifest(
            r#"
[[extension]]
name = "my-linter"
command = "my-linter-ext"
args = ["--serve"]
events = ["tool.before", "tool.after"]
provides_tools = true
timeout_ms = 5000
"#,
        );
        let manifest = PluginManifest::from_toml(&toml_str).expect("valid extension should parse");

        assert_eq!(manifest.extensions.len(), 1);
        let extension = &manifest.extensions[0];
        assert_eq!(extension.name, "my-linter");
        assert_eq!(extension.command, "my-linter-ext");
        assert_eq!(extension.args, vec!["--serve".to_string()]);
        assert_eq!(
            extension.events,
            vec!["tool.before".to_string(), "tool.after".to_string()]
        );
        assert!(extension.provides_tools);
        assert_eq!(extension.timeout_ms, Some(5000));
    }

    #[test]
    fn extension_optional_fields_default_when_omitted() {
        let toml_str = minimal_manifest(
            r#"
[[extension]]
name = "bare"
command = "bare-ext"
"#,
        );
        let manifest =
            PluginManifest::from_toml(&toml_str).expect("minimal extension should parse");

        let extension = &manifest.extensions[0];
        assert!(extension.args.is_empty());
        assert!(extension.events.is_empty());
        assert!(!extension.provides_tools);
        assert_eq!(extension.timeout_ms, None);
    }

    #[test]
    fn manifest_without_extensions_still_validates() {
        let toml_str = minimal_manifest("");
        let manifest =
            PluginManifest::from_toml(&toml_str).expect("no extensions should still validate");
        assert!(manifest.extensions.is_empty());
    }

    #[test]
    fn rejects_duplicate_extension_names() {
        let toml_str = minimal_manifest(
            r#"
[[extension]]
name = "dup"
command = "one"

[[extension]]
name = "dup"
command = "two"
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("duplicate extension names should fail validation");
        assert!(matches!(err, ManifestError::DuplicateExtensionName(name) if name == "dup"));
    }

    #[test]
    fn extension_name_may_collide_with_an_mcp_server_name() {
        // Extensions and MCP servers are separate capability axes wired
        // independently by ryuzi-core's PluginHost, so they occupy separate
        // name namespaces within one manifest.
        let toml_str = minimal_manifest(
            r#"
[[mcp]]
name = "shared"
transport = "stdio"
command = "npx"

[[extension]]
name = "shared"
command = "shared-ext"
"#,
        );
        let manifest = PluginManifest::from_toml(&toml_str)
            .expect("extension and mcp server may share a name");
        assert_eq!(manifest.mcp[0].name, "shared");
        assert_eq!(manifest.extensions[0].name, "shared");
    }

    #[test]
    fn rejects_extension_with_empty_command() {
        let toml_str = minimal_manifest(
            r#"
[[extension]]
name = "no-command"
command = ""
"#,
        );
        let err =
            PluginManifest::from_toml(&toml_str).expect_err("empty command should fail validation");
        assert!(matches!(err, ManifestError::ExtensionEmptyCommand(name) if name == "no-command"));
    }

    #[test]
    fn rejects_extension_with_unknown_event() {
        let toml_str = minimal_manifest(
            r#"
[[extension]]
name = "typo-event"
command = "ext"
events = ["tool.before", "tool.beforee"]
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("unknown hook event should fail validation");
        assert!(matches!(
            err,
            ManifestError::ExtensionUnknownEvent(name, event)
                if name == "typo-event" && event == "tool.beforee"
        ));
    }

    #[test]
    fn rejects_extension_timeout_ms_of_zero() {
        let toml_str = minimal_manifest(
            r#"
[[extension]]
name = "zero-timeout"
command = "ext"
timeout_ms = 0
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("zero timeout_ms should fail validation");
        assert!(matches!(
            err,
            ManifestError::ExtensionTimeoutOutOfRange(name, timeout_ms)
                if name == "zero-timeout" && timeout_ms == 0
        ));
    }

    #[test]
    fn rejects_extension_timeout_ms_above_cap() {
        let toml_str = minimal_manifest(
            r#"
[[extension]]
name = "huge-timeout"
command = "ext"
timeout_ms = 60001
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("timeout_ms above the cap should fail validation");
        assert!(matches!(
            err,
            ManifestError::ExtensionTimeoutOutOfRange(name, timeout_ms)
                if name == "huge-timeout" && timeout_ms == 60001
        ));
    }

    #[test]
    fn extension_timeout_ms_at_cap_is_allowed() {
        let toml_str = minimal_manifest(
            r#"
[[extension]]
name = "at-cap"
command = "ext"
timeout_ms = 60000
"#,
        );
        let manifest = PluginManifest::from_toml(&toml_str)
            .expect("timeout_ms exactly at the cap should validate");
        assert_eq!(manifest.extensions[0].timeout_ms, Some(60000));
    }

    #[test]
    fn rejects_extension_auth_placeholder_without_auth_block() {
        let toml_str = minimal_manifest(
            r#"
[[extension]]
name = "needs-auth"
command = "ext"
args = ["${auth}"]
"#,
        );
        let err = PluginManifest::from_toml(&toml_str)
            .expect_err("${auth} without [auth] should fail validation");
        assert!(
            matches!(err, ManifestError::ExtensionAuthPlaceholderWithoutAuth(name) if name == "needs-auth")
        );
    }

    #[test]
    fn extension_auth_placeholder_with_auth_block_parses() {
        let toml_str = minimal_manifest(
            r#"
[auth]
kind = "token"

[[extension]]
name = "has-auth"
command = "${auth}"
"#,
        );
        let manifest = PluginManifest::from_toml(&toml_str)
            .expect("${auth} with an [auth] block should validate");
        assert_eq!(manifest.extensions[0].command, "${auth}");
    }
}
