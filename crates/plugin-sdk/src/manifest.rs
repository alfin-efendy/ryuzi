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
    #[serde(default)]
    pub skills: Vec<SkillDef>,
    #[serde(default)]
    pub menu: Option<MenuContribution>,
    #[serde(default)]
    pub provider: Option<ProviderMeta>,
    #[serde(default)]
    pub runtime: Option<RuntimeMeta>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SkillDef {
    pub name: String,
    #[serde(default)]
    pub description: String,
    pub path: String,
}

fn default_section() -> String {
    "plugins".to_string()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MenuContribution {
    #[serde(default = "default_section")]
    pub section: String,
    #[serde(default)]
    pub label: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeMeta {
    #[serde(default)]
    pub binary: Option<String>,
    #[serde(default)]
    pub npm_package: Option<String>,
    #[serde(default)]
    pub default_model: Option<String>,
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

        Ok(())
    }

    /// Non-fatal feedback: categories outside the standard vocabulary
    /// (`categories::KNOWN`). Unlike `validate()`, this never rejects a
    /// manifest — new categories should not break the loader.
    pub fn warnings(&self) -> Vec<String> {
        self.categories
            .iter()
            .filter(|category| !categories::KNOWN.contains(&category.as_str()))
            .cloned()
            .collect()
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

[menu]
section = "plugins"
label = "GitHub"
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

        let menu = manifest.menu.expect("menu block");
        assert_eq!(menu.section, "plugins");
        assert_eq!(menu.label.as_deref(), Some("GitHub"));
    }

    #[test]
    fn round_trips_provider_and_runtime_blocks() {
        let toml_str = r#"
contract = 1
id = "anthropic"
name = "Anthropic"

[provider]
format = "anthropic"
base_url = "https://api.anthropic.com"
models = [ { id = "claude-opus-4-5", label = "Opus 4.5", default = true } ]

[runtime]
binary = "claude"
npm_package = "@anthropic-ai/claude-code"
default_model = "claude-opus-4-5"
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

        let runtime = manifest.runtime.expect("runtime block");
        assert_eq!(runtime.binary.as_deref(), Some("claude"));
        assert_eq!(
            runtime.npm_package.as_deref(),
            Some("@anthropic-ai/claude-code")
        );
        assert_eq!(runtime.default_model.as_deref(), Some("claude-opus-4-5"));
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
}
