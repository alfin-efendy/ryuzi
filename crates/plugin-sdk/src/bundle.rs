//! The plugin *component bundle* contract: the declarative shape of a
//! Wasm-component plugin release (`ryuzi-plugin-bundle.toml`) and the
//! release descriptor a registry serves for it (JSON). This module owns
//! parsing and structural validation only — like `crate::manifest`, it has
//! no opinion on how a bundle becomes a running Wasmtime component; that
//! binding lives in `ryuzi-core` (not implemented here).
//!
//! Deliberately runtime-independent: no Wasmtime, no installer, no network
//! I/O. `PluginBundleManifest` and `PluginRelease` are data + validation
//! only, mirroring `crate::manifest::PluginManifest`'s discipline.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

/// One Wasm-component plugin bundle: identity, the WIT contract range it
/// targets, its instancing lifecycle, the component binary filename, and
/// its permission contract (network allowlist, OAuth profiles). Authored as
/// TOML (`ryuzi-plugin-bundle.toml`) alongside the compiled `.wasm`
/// component.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginBundleManifest {
    pub id: String,
    pub name: String,
    /// This bundle's own release version. Must parse as a
    /// [`semver::Version`] (e.g. `0.1.0`).
    pub version: String,
    /// The WIT contract version range this bundle's component targets, as
    /// a Cargo-style [`semver::VersionReq`] (e.g. `^0.1.0`). WIT contracts
    /// begin at `0.1.0`; this crate does not ship WIT files itself.
    #[serde(rename = "wit-api")]
    pub wit_api: String,
    pub lifecycle: PluginLifecycle,
    /// The compiled component's filename, relative to the bundle root
    /// (e.g. `acme_connector.wasm`). Required and must be non-empty.
    pub component: String,
    #[serde(default)]
    pub publisher: String,
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub permissions: PluginPermissions,
    #[serde(default)]
    pub oauth: Vec<OAuthProfile>,
    /// The LLM-router provider id(s) a provider bundle serves. A provider
    /// component often serves a router id that differs from its bundle `id`
    /// (e.g. the `mimo` bundle backs the `mimo-free` router provider), so this
    /// lets the bundle DECLARE those ids rather than the host hardcoding a
    /// `bundle-id -> provider-id` mapping. Optional and empty by default: an
    /// existing manifest that omits it keeps working, falling back to `[id]`
    /// via [`PluginBundleManifest::resolved_provider_ids`]. Ignored for
    /// non-provider bundles.
    ///
    /// Declaring this EXPLICITLY is also what lets the host grant the bundle
    /// the `ryuzi:provider-auth` capability (host-injected user API keys) — and
    /// it bounds which providers' credentials the bundle may use. The `[id]`
    /// fallback exists only for transport registration and never authorizes a
    /// credential.
    #[serde(default, rename = "provider-ids")]
    pub provider_ids: Vec<String>,
}

/// How the host instances a bundle's component: one shared instance for
/// the whole process, one instance per session, or a fresh instance per
/// call. Purely declarative here — the instancing policy itself is
/// enforced by the (not-yet-implemented) Wasmtime host.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PluginLifecycle {
    Singleton,
    PerSession,
    PerCall,
}

/// A bundle's permission contract. Currently just the outbound network
/// allowlist; more permission axes (filesystem, env, secrets) can be added
/// as new fields without breaking existing bundles (`#[serde(default)]`).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct PluginPermissions {
    pub network: Vec<NetworkPermission>,
}

/// One outbound-network allowlist entry: a bare lowercase hostname
/// (`api.github.com`) or a `*.`-prefixed wildcard hostname
/// (`*.github.com`). No scheme, path, port, IP literal, bare `*`, or
/// uppercase — see the host-validation logic exercised by
/// [`PluginBundleManifest::validate`] for the exact grammar enforced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NetworkPermission(pub String);

/// One OAuth profile a bundle's component may use to authenticate. A
/// bundle may declare more than one (e.g. a connector that talks to two
/// different OAuth-protected APIs); `id` must be unique within the bundle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(default, rename_all = "kebab-case")]
pub struct OAuthProfile {
    pub id: String,
    pub authorize_url: Option<String>,
    pub token_url: Option<String>,
    pub scopes: Vec<String>,
    pub client_id_setting: Option<String>,
    pub client_secret_setting: Option<String>,
    pub resource: Option<String>,
    pub dynamic_registration: bool,
}

/// A registry's release descriptor for a bundle: the concrete WIT API
/// version it was built against, where to fetch the component, and its
/// checksum. Served as JSON (unlike the TOML-authored
/// [`PluginBundleManifest`]) since it is generated by a registry, not
/// hand-authored by a plugin publisher.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PluginRelease {
    pub id: String,
    /// This release's version. Must parse as a [`semver::Version`].
    pub version: String,
    /// The exact WIT contract version this release's component was built
    /// against. Unlike [`PluginBundleManifest::wit_api`] (a range), this is
    /// a single concrete [`semver::Version`].
    #[serde(rename = "wit-api")]
    pub wit_api: String,
    pub component_url: String,
    pub component_sha256: String,
    #[serde(default)]
    pub size_bytes: Option<u64>,
    #[serde(default)]
    pub published_at: Option<String>,
}

/// Errors from parsing or validating a [`PluginBundleManifest`] or
/// [`PluginRelease`].
#[derive(Debug, thiserror::Error)]
pub enum BundleError {
    #[error("invalid plugin bundle manifest toml: {0}")]
    Toml(#[from] toml::de::Error),
    #[error("invalid plugin release json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid plugin id: {0}")]
    InvalidId(String),
    #[error("plugin name must not be empty")]
    EmptyName,
    #[error("invalid version {0:?}: {1}")]
    InvalidVersion(String, String),
    #[error("invalid wit-api version {0:?}: {1}")]
    InvalidWitApi(String, String),
    #[error("component filename must not be empty")]
    EmptyComponent,
    #[error("component_url must not be empty")]
    EmptyComponentUrl,
    #[error("component_sha256 must not be empty")]
    EmptyComponentSha256,
    #[error("invalid network allowlist entry: {0:?}")]
    InvalidNetworkHost(String),
    #[error("oauth profile id must not be empty")]
    EmptyOAuthProfileId,
    #[error("duplicate oauth profile id: {0}")]
    DuplicateOAuthProfile(String),
    #[error("invalid provider id: {0:?}")]
    InvalidProviderId(String),
}

fn is_valid_id(id: &str) -> bool {
    let mut chars = id.chars();
    match chars.next() {
        Some(c) if c.is_ascii_lowercase() || c.is_ascii_digit() => {}
        _ => return false,
    }
    chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `true` if `host` is a bare lowercase hostname (`api.github.com`) or a
/// `*.`-prefixed wildcard hostname (`*.github.com`). Rejects a scheme
/// (`://`), a path or port (`/`, `:`), whitespace, an IP literal, a bare
/// `*`, a wildcard anywhere but the leading `*.`, uppercase characters, and
/// blank input.
fn is_valid_network_host(host: &str) -> bool {
    if host.is_empty() {
        return false;
    }
    if host.contains("://") {
        return false;
    }

    let body = match host.strip_prefix("*.") {
        Some(rest) if !rest.is_empty() => rest,
        Some(_) => return false, // "*." with nothing after it
        None => host,
    };

    if body.contains('*') || body.contains('/') || body.contains(':') || body.contains(' ') {
        return false;
    }
    if body.chars().any(|c| c.is_ascii_uppercase()) {
        return false;
    }
    if body.parse::<std::net::IpAddr>().is_ok() {
        return false;
    }

    let labels: Vec<&str> = body.split('.').collect();
    labels.iter().all(|label| {
        !label.is_empty()
            && !label.starts_with('-')
            && !label.ends_with('-')
            && label.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
    })
}

impl PluginBundleManifest {
    /// Parse TOML into a bundle manifest and validate it in one step.
    pub fn from_toml(input: &str) -> Result<PluginBundleManifest, BundleError> {
        let bundle: PluginBundleManifest = toml::from_str(input)?;
        bundle.validate()?;
        Ok(bundle)
    }

    /// The LLM-router provider id(s) this bundle serves: the declared
    /// [`PluginBundleManifest::provider_ids`] when non-empty, otherwise the
    /// single-element fallback `[self.id]`. The host registers one provider
    /// transport per returned id, so a bundle whose router id differs from its
    /// bundle id (e.g. `mimo` -> `mimo-free`) is resolved generically from the
    /// manifest, never a host-side id branch.
    pub fn resolved_provider_ids(&self) -> Vec<String> {
        if self.provider_ids.is_empty() {
            vec![self.id.clone()]
        } else {
            self.provider_ids.clone()
        }
    }

    /// Structural validation: id shape, required fields, `version` and
    /// `wit-api` semver parsing, non-empty `component`, a well-formed
    /// network allowlist, and unique OAuth profile ids.
    pub fn validate(&self) -> Result<(), BundleError> {
        if !is_valid_id(&self.id) {
            return Err(BundleError::InvalidId(self.id.clone()));
        }
        if self.name.is_empty() {
            return Err(BundleError::EmptyName);
        }
        semver::Version::parse(&self.version)
            .map_err(|e| BundleError::InvalidVersion(self.version.clone(), e.to_string()))?;
        semver::VersionReq::parse(&self.wit_api)
            .map_err(|e| BundleError::InvalidWitApi(self.wit_api.clone(), e.to_string()))?;
        if self.component.is_empty() {
            return Err(BundleError::EmptyComponent);
        }

        for entry in &self.permissions.network {
            if !is_valid_network_host(&entry.0) {
                return Err(BundleError::InvalidNetworkHost(entry.0.clone()));
            }
        }

        let mut seen_oauth_ids: HashSet<&str> = HashSet::new();
        for profile in &self.oauth {
            if profile.id.is_empty() {
                return Err(BundleError::EmptyOAuthProfileId);
            }
            if !seen_oauth_ids.insert(profile.id.as_str()) {
                return Err(BundleError::DuplicateOAuthProfile(profile.id.clone()));
            }
        }

        // A declared router provider id must be a well-formed id (the same
        // lowercase/digit/hyphen grammar as the bundle `id`): non-empty, no
        // whitespace, no uppercase. Empty is allowed (the whole list is
        // optional); each present entry is checked.
        for provider_id in &self.provider_ids {
            if !is_valid_id(provider_id) {
                return Err(BundleError::InvalidProviderId(provider_id.clone()));
            }
        }

        Ok(())
    }
}

impl PluginRelease {
    /// Parse JSON into a release descriptor and validate it in one step.
    pub fn from_json(input: &[u8]) -> Result<PluginRelease, BundleError> {
        let release: PluginRelease = serde_json::from_slice(input)?;
        release.validate()?;
        Ok(release)
    }

    /// Structural validation: id shape, `version` and `wit-api` semver
    /// parsing, and non-empty download coordinates.
    pub fn validate(&self) -> Result<(), BundleError> {
        if !is_valid_id(&self.id) {
            return Err(BundleError::InvalidId(self.id.clone()));
        }
        semver::Version::parse(&self.version)
            .map_err(|e| BundleError::InvalidVersion(self.version.clone(), e.to_string()))?;
        semver::Version::parse(&self.wit_api)
            .map_err(|e| BundleError::InvalidWitApi(self.wit_api.clone(), e.to_string()))?;
        if self.component_url.is_empty() {
            return Err(BundleError::EmptyComponentUrl);
        }
        if self.component_sha256.is_empty() {
            return Err(BundleError::EmptyComponentSha256);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn valid_connector_bundle() -> String {
        r#"
id = "acme-connector"
name = "Acme Connector"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "singleton"
component = "acme_connector.wasm"
publisher = "Acme Inc."
description = "Talks to the Acme API."

[permissions]
network = ["api.github.com", "*.github.com"]

[[oauth]]
id = "github"
authorize-url = "https://github.com/login/oauth/authorize"
token-url = "https://github.com/login/oauth/access_token"
scopes = ["repo"]
"#
        .to_string()
    }

    #[test]
    fn parses_a_valid_connector_bundle() {
        let bundle = PluginBundleManifest::from_toml(&valid_connector_bundle())
            .expect("valid bundle should parse and validate");
        assert_eq!(bundle.id, "acme-connector");
        assert_eq!(bundle.name, "Acme Connector");
        assert_eq!(bundle.version, "0.1.0");
        assert_eq!(bundle.wit_api, "^0.1.0");
        assert_eq!(bundle.lifecycle, PluginLifecycle::Singleton);
        assert_eq!(bundle.component, "acme_connector.wasm");
        assert_eq!(
            bundle.permissions.network,
            vec![
                NetworkPermission("api.github.com".to_string()),
                NetworkPermission("*.github.com".to_string()),
            ]
        );
        assert_eq!(bundle.oauth.len(), 1);
        assert_eq!(bundle.oauth[0].id, "github");
        assert_eq!(bundle.oauth[0].scopes, vec!["repo".to_string()]);
    }

    #[test]
    fn minimal_bundle_without_permissions_or_oauth_parses() {
        let toml_str = r#"
id = "acme"
name = "Acme"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "per-session"
component = "acme.wasm"
"#;
        let bundle =
            PluginBundleManifest::from_toml(toml_str).expect("minimal bundle should validate");
        assert_eq!(bundle.lifecycle, PluginLifecycle::PerSession);
        assert!(bundle.permissions.network.is_empty());
        assert!(bundle.oauth.is_empty());
    }

    #[test]
    fn per_call_lifecycle_parses() {
        let toml_str = r#"
id = "acme"
name = "Acme"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "per-call"
component = "acme.wasm"
"#;
        let bundle = PluginBundleManifest::from_toml(toml_str).expect("per-call should validate");
        assert_eq!(bundle.lifecycle, PluginLifecycle::PerCall);
    }

    #[test]
    fn rejects_missing_component() {
        let toml_str = r#"
id = "acme"
name = "Acme"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "singleton"
"#;
        let err = PluginBundleManifest::from_toml(toml_str)
            .expect_err("missing component should fail to parse");
        assert!(matches!(err, BundleError::Toml(_)));
    }

    #[test]
    fn rejects_empty_component_filename() {
        let toml_str = r#"
id = "acme"
name = "Acme"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "singleton"
component = ""
"#;
        let err = PluginBundleManifest::from_toml(toml_str)
            .expect_err("empty component filename should fail validation");
        assert!(matches!(err, BundleError::EmptyComponent));
    }

    #[test]
    fn rejects_invalid_id() {
        let toml_str = r#"
id = "Acme"
name = "Acme"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "singleton"
component = "acme.wasm"
"#;
        let err = PluginBundleManifest::from_toml(toml_str)
            .expect_err("uppercase id should fail validation");
        assert!(matches!(err, BundleError::InvalidId(id) if id == "Acme"));
    }

    #[test]
    fn rejects_empty_name() {
        let toml_str = r#"
id = "acme"
name = ""
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "singleton"
component = "acme.wasm"
"#;
        let err = PluginBundleManifest::from_toml(toml_str)
            .expect_err("empty name should fail validation");
        assert!(matches!(err, BundleError::EmptyName));
    }

    #[test]
    fn rejects_invalid_semver_version() {
        let toml_str = r#"
id = "acme"
name = "Acme"
version = "not-a-version"
wit-api = "^0.1.0"
lifecycle = "singleton"
component = "acme.wasm"
"#;
        let err = PluginBundleManifest::from_toml(toml_str)
            .expect_err("invalid semver version should fail validation");
        assert!(matches!(err, BundleError::InvalidVersion(v, _) if v == "not-a-version"));
    }

    #[test]
    fn rejects_version_missing_patch_component() {
        // semver::Version requires major.minor.patch; "0.1" is not valid.
        let toml_str = r#"
id = "acme"
name = "Acme"
version = "0.1"
wit-api = "^0.1.0"
lifecycle = "singleton"
component = "acme.wasm"
"#;
        let err = PluginBundleManifest::from_toml(toml_str)
            .expect_err("version without a patch component should fail validation");
        assert!(matches!(err, BundleError::InvalidVersion(v, _) if v == "0.1"));
    }

    #[test]
    fn rejects_invalid_wit_api_version_req() {
        let toml_str = r#"
id = "acme"
name = "Acme"
version = "0.1.0"
wit-api = "not-a-range"
lifecycle = "singleton"
component = "acme.wasm"
"#;
        let err = PluginBundleManifest::from_toml(toml_str)
            .expect_err("invalid wit-api version req should fail validation");
        assert!(matches!(err, BundleError::InvalidWitApi(v, _) if v == "not-a-range"));
    }

    #[test]
    fn rejects_duplicate_oauth_profile_ids() {
        let toml_str = r#"
id = "acme"
name = "Acme"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "singleton"
component = "acme.wasm"

[[oauth]]
id = "github"

[[oauth]]
id = "github"
"#;
        let err = PluginBundleManifest::from_toml(toml_str)
            .expect_err("duplicate oauth profile id should fail validation");
        assert!(matches!(err, BundleError::DuplicateOAuthProfile(id) if id == "github"));
    }

    #[test]
    fn rejects_empty_oauth_profile_id() {
        let toml_str = r#"
id = "acme"
name = "Acme"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "singleton"
component = "acme.wasm"

[[oauth]]
id = ""
"#;
        let err = PluginBundleManifest::from_toml(toml_str)
            .expect_err("empty oauth profile id should fail validation");
        assert!(matches!(err, BundleError::EmptyOAuthProfileId));
    }

    #[test]
    fn parses_declared_provider_ids() {
        let toml_str = r#"
id = "mimo"
name = "MiMo"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "per-call"
component = "mimo.wasm"
provider-ids = ["mimo-free"]
"#;
        let bundle = PluginBundleManifest::from_toml(toml_str)
            .expect("a bundle declaring provider-ids should validate");
        assert_eq!(bundle.provider_ids, vec!["mimo-free".to_string()]);
        assert_eq!(
            bundle.resolved_provider_ids(),
            vec!["mimo-free".to_string()]
        );
    }

    #[test]
    fn provider_ids_default_to_the_manifest_id_when_absent() {
        // A pre-existing manifest with no `provider-ids` key keeps working: the
        // field defaults to empty and the accessor falls back to `[id]`.
        let toml_str = r#"
id = "acme"
name = "Acme"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "singleton"
component = "acme.wasm"
"#;
        let bundle = PluginBundleManifest::from_toml(toml_str)
            .expect("a manifest without provider-ids must still validate");
        assert!(bundle.provider_ids.is_empty());
        assert_eq!(bundle.resolved_provider_ids(), vec!["acme".to_string()]);
    }

    #[test]
    fn resolved_provider_ids_honors_multiple_declared_entries() {
        let toml_str = r#"
id = "multi"
name = "Multi"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "per-call"
component = "multi.wasm"
provider-ids = ["one-free", "two-free"]
"#;
        let bundle = PluginBundleManifest::from_toml(toml_str)
            .expect("multiple provider-ids should validate");
        assert_eq!(
            bundle.resolved_provider_ids(),
            vec!["one-free".to_string(), "two-free".to_string()]
        );
    }

    #[test]
    fn rejects_invalid_provider_id() {
        let toml_str = r#"
id = "mimo"
name = "MiMo"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "per-call"
component = "mimo.wasm"
provider-ids = ["Mimo Free"]
"#;
        let err = PluginBundleManifest::from_toml(toml_str)
            .expect_err("an uppercase/whitespace provider id should fail validation");
        assert!(matches!(err, BundleError::InvalidProviderId(id) if id == "Mimo Free"));
    }

    fn bundle_with_network(host: &str) -> String {
        format!(
            r#"
id = "acme"
name = "Acme"
version = "0.1.0"
wit-api = "^0.1.0"
lifecycle = "singleton"
component = "acme.wasm"

[permissions]
network = ["{host}"]
"#
        )
    }

    #[test]
    fn accepts_a_bare_hostname() {
        let toml_str = bundle_with_network("api.github.com");
        let bundle =
            PluginBundleManifest::from_toml(&toml_str).expect("bare hostname should validate");
        assert_eq!(bundle.permissions.network[0].0, "api.github.com");
    }

    #[test]
    fn accepts_a_wildcard_hostname() {
        let toml_str = bundle_with_network("*.github.com");
        let bundle =
            PluginBundleManifest::from_toml(&toml_str).expect("wildcard hostname should validate");
        assert_eq!(bundle.permissions.network[0].0, "*.github.com");
    }

    #[test]
    fn rejects_network_host_with_scheme() {
        let toml_str = bundle_with_network("https://api.github.com");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("scheme in network host should fail validation");
        assert!(matches!(err, BundleError::InvalidNetworkHost(h) if h == "https://api.github.com"));
    }

    #[test]
    fn rejects_network_host_with_scheme_and_path() {
        let toml_str = bundle_with_network("https://api.github.com/v3");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("scheme + path in network host should fail validation");
        assert!(
            matches!(err, BundleError::InvalidNetworkHost(h) if h == "https://api.github.com/v3")
        );
    }

    #[test]
    fn rejects_network_host_with_bare_path() {
        let toml_str = bundle_with_network("api.github.com/v3");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("path in network host should fail validation");
        assert!(matches!(err, BundleError::InvalidNetworkHost(h) if h == "api.github.com/v3"));
    }

    #[test]
    fn rejects_network_host_with_port() {
        let toml_str = bundle_with_network("api.github.com:443");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("port in network host should fail validation");
        assert!(matches!(err, BundleError::InvalidNetworkHost(h) if h == "api.github.com:443"));
    }

    #[test]
    fn rejects_network_host_that_is_an_ip_literal() {
        let toml_str = bundle_with_network("192.168.1.1");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("IP literal network host should fail validation");
        assert!(matches!(err, BundleError::InvalidNetworkHost(h) if h == "192.168.1.1"));
    }

    #[test]
    fn rejects_network_host_that_is_an_ipv6_literal() {
        let toml_str = bundle_with_network("::1");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("IPv6 literal network host should fail validation");
        assert!(matches!(err, BundleError::InvalidNetworkHost(h) if h == "::1"));
    }

    #[test]
    fn rejects_bare_wildcard_network_host() {
        let toml_str = bundle_with_network("*");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("bare wildcard network host should fail validation");
        assert!(matches!(err, BundleError::InvalidNetworkHost(h) if h == "*"));
    }

    #[test]
    fn rejects_wildcard_with_nothing_after_the_dot() {
        let toml_str = bundle_with_network("*.");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("dangling wildcard suffix should fail validation");
        assert!(matches!(err, BundleError::InvalidNetworkHost(h) if h == "*."));
    }

    #[test]
    fn rejects_wildcard_not_at_the_leading_position() {
        let toml_str = bundle_with_network("api.*.github.com");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("mid-string wildcard should fail validation");
        assert!(matches!(err, BundleError::InvalidNetworkHost(h) if h == "api.*.github.com"));
    }

    #[test]
    fn rejects_wildcard_without_a_dot_separator() {
        let toml_str = bundle_with_network("*github.com");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("wildcard without a dot separator should fail validation");
        assert!(matches!(err, BundleError::InvalidNetworkHost(h) if h == "*github.com"));
    }

    #[test]
    fn rejects_uppercase_network_host() {
        let toml_str = bundle_with_network("API.github.com");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("uppercase network host should fail validation");
        assert!(matches!(err, BundleError::InvalidNetworkHost(h) if h == "API.github.com"));
    }

    #[test]
    fn rejects_blank_network_host() {
        let toml_str = bundle_with_network("");
        let err = PluginBundleManifest::from_toml(&toml_str)
            .expect_err("blank network host should fail validation");
        assert!(matches!(err, BundleError::InvalidNetworkHost(h) if h.is_empty()));
    }

    fn valid_release_json() -> Vec<u8> {
        br#"{
            "id": "acme-connector",
            "version": "0.1.0",
            "wit-api": "0.1.0",
            "component_url": "https://registry.example.com/acme-connector/0.1.0/acme_connector.wasm",
            "component_sha256": "deadbeef",
            "size_bytes": 1024,
            "published_at": "2026-07-12T00:00:00Z"
        }"#
        .to_vec()
    }

    #[test]
    fn parses_a_valid_release() {
        let release =
            PluginRelease::from_json(&valid_release_json()).expect("valid release should parse");
        assert_eq!(release.id, "acme-connector");
        assert_eq!(release.version, "0.1.0");
        assert_eq!(release.wit_api, "0.1.0");
        assert_eq!(
            release.component_url,
            "https://registry.example.com/acme-connector/0.1.0/acme_connector.wasm"
        );
        assert_eq!(release.component_sha256, "deadbeef");
        assert_eq!(release.size_bytes, Some(1024));
        assert_eq!(
            release.published_at.as_deref(),
            Some("2026-07-12T00:00:00Z")
        );
    }

    #[test]
    fn release_size_bytes_and_published_at_are_optional() {
        let json = br#"{
            "id": "acme-connector",
            "version": "0.1.0",
            "wit-api": "0.1.0",
            "component_url": "https://registry.example.com/a.wasm",
            "component_sha256": "deadbeef"
        }"#;
        let release = PluginRelease::from_json(json).expect("minimal release should parse");
        assert_eq!(release.size_bytes, None);
        assert_eq!(release.published_at, None);
    }

    #[test]
    fn rejects_malformed_release_json() {
        let err =
            PluginRelease::from_json(b"not json").expect_err("malformed json should fail to parse");
        assert!(matches!(err, BundleError::Json(_)));
    }

    #[test]
    fn rejects_release_with_invalid_semver_version() {
        let json = br#"{
            "id": "acme-connector",
            "version": "not-a-version",
            "wit-api": "0.1.0",
            "component_url": "https://registry.example.com/a.wasm",
            "component_sha256": "deadbeef"
        }"#;
        let err = PluginRelease::from_json(json)
            .expect_err("invalid semver version should fail validation");
        assert!(matches!(err, BundleError::InvalidVersion(v, _) if v == "not-a-version"));
    }

    #[test]
    fn rejects_release_with_invalid_wit_api_version() {
        let json = br#"{
            "id": "acme-connector",
            "version": "0.1.0",
            "wit-api": "^0.1.0",
            "component_url": "https://registry.example.com/a.wasm",
            "component_sha256": "deadbeef"
        }"#;
        let err = PluginRelease::from_json(json)
            .expect_err("wit-api must be a concrete version, not a range");
        assert!(matches!(err, BundleError::InvalidWitApi(v, _) if v == "^0.1.0"));
    }

    #[test]
    fn rejects_release_with_empty_component_url() {
        let json = br#"{
            "id": "acme-connector",
            "version": "0.1.0",
            "wit-api": "0.1.0",
            "component_url": "",
            "component_sha256": "deadbeef"
        }"#;
        let err =
            PluginRelease::from_json(json).expect_err("empty component_url should fail validation");
        assert!(matches!(err, BundleError::EmptyComponentUrl));
    }

    #[test]
    fn rejects_release_with_empty_component_sha256() {
        let json = br#"{
            "id": "acme-connector",
            "version": "0.1.0",
            "wit-api": "0.1.0",
            "component_url": "https://registry.example.com/a.wasm",
            "component_sha256": ""
        }"#;
        let err = PluginRelease::from_json(json)
            .expect_err("empty component_sha256 should fail validation");
        assert!(matches!(err, BundleError::EmptyComponentSha256));
    }

    #[test]
    fn rejects_release_with_invalid_id() {
        let json = br#"{
            "id": "Acme Connector",
            "version": "0.1.0",
            "wit-api": "0.1.0",
            "component_url": "https://registry.example.com/a.wasm",
            "component_sha256": "deadbeef"
        }"#;
        let err = PluginRelease::from_json(json).expect_err("invalid id should fail validation");
        assert!(matches!(err, BundleError::InvalidId(id) if id == "Acme Connector"));
    }
}
