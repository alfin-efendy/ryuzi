//! Declarative plugins: turn a `PluginManifest`'s `[[mcp]]` entries into a
//! working `Connector` via placeholder substitution — no bespoke Rust code
//! required per plugin. Used both for manifest-authored builtins/catalog
//! entries and for user plugins discovered from disk
//! (`plugins::load_user_plugins`).

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use async_trait::async_trait;
use ryuzi_plugin_sdk::subst::{resolve, Resolver};
use ryuzi_plugin_sdk::{AuthKind, McpServerDef, McpTransportDef, PluginManifest};

use crate::connector::{Connector, ConnectorCtx};
use crate::domain::{McpServerSpec, McpTransport};

use super::host::{CorePlugin, PluginSource};

/// Build a `CorePlugin` from a manifest. Harness and gateway capability can
/// never come from a manifest alone (those require Rust code — see
/// `harness::native`, `harness::acp`, `plugins::builtin::discord_plugin`);
/// the only capability a declarative plugin can carry is a connector, and
/// only when `manifest.mcp` is non-empty.
pub fn declarative_plugin(
    manifest: PluginManifest,
    source: PluginSource,
) -> anyhow::Result<CorePlugin> {
    manifest.validate()?;
    let connector: Option<Arc<dyn Connector>> = if manifest.mcp.is_empty() {
        None
    } else {
        Some(Arc::new(DeclarativeConnector {
            manifest: manifest.clone(),
        }))
    };
    Ok(CorePlugin {
        manifest,
        harness: None,
        gateway: None,
        connector,
        source,
    })
}

/// A connector whose entire behavior is "substitute placeholders into this
/// manifest's `[[mcp]]` entries" — no bespoke Rust per plugin author.
struct DeclarativeConnector {
    manifest: PluginManifest,
}

#[async_trait]
impl Connector for DeclarativeConnector {
    async fn mcp_servers(&self, ctx: &ConnectorCtx) -> anyhow::Result<Vec<McpServerSpec>> {
        let resolver = self.resolver(ctx).await?;
        self.manifest
            .mcp
            .iter()
            .map(|server| build_spec(server, &resolver))
            .collect()
    }

    async fn ensure_auth(&self, ctx: &ConnectorCtx) -> anyhow::Result<()> {
        if let Some(auth) = &self.manifest.auth {
            // NOTE: `auth.kind = "none"` only means ensure_auth never
            // *requires* a credential — if the manifest still populates
            // `auth.setting` or `auth.env`, `${auth}` substitution
            // (`resolve_auth`, below) may still resolve a value from those
            // sources and inject it into an `[[mcp]]` entry. "none" is about
            // the gate, not about whether a value exists.
            if auth.kind != AuthKind::None && self.resolve_auth(ctx).await?.is_none() {
                let id = &self.manifest.id;
                match &auth.help_url {
                    Some(url) => anyhow::bail!("configure {id}: see {url}"),
                    None => anyhow::bail!("configure {id}: missing credentials"),
                }
            }
        }
        // Beyond the (auth-specific) credential above, some manifests
        // declare non-auth `[[settings]]` fields that an `[[mcp]]` entry's
        // `${setting:KEY}` placeholder depends on to function at all (e.g.
        // honcho's `plugin.honcho.user` header, datadog's
        // `plugin.datadog.app_key` header) — `required = true` marks those.
        // A missing one would otherwise only surface once the MCP server
        // rejects an empty/absent header, so check it up front here too,
        // with the same friendly "name the plugin + key (+ help_url)" shape
        // as the auth error above.
        for field in &self.manifest.settings {
            if !field.required {
                continue;
            }
            let present = ctx
                .settings
                .get(&field.key)
                .await?
                .is_some_and(|v| !v.is_empty());
            if present {
                continue;
            }
            let id = &self.manifest.id;
            let key = &field.key;
            match self
                .manifest
                .auth
                .as_ref()
                .and_then(|a| a.help_url.as_ref())
            {
                Some(url) => {
                    anyhow::bail!("configure {id}: missing required setting {key} — see {url}")
                }
                None => anyhow::bail!("configure {id}: missing required setting {key}"),
            }
        }
        Ok(())
    }
}

impl DeclarativeConnector {
    /// Resolve the manifest's `[auth]` value: the settings row named by
    /// `auth.setting` if present and non-empty, else the process env var
    /// named by `auth.env` if present and non-empty. Never logged — only
    /// the presence/absence of a value ever surfaces (in `ensure_auth`'s
    /// error message, which names keys, not values).
    async fn resolve_auth(&self, ctx: &ConnectorCtx) -> anyhow::Result<Option<String>> {
        let Some(auth) = &self.manifest.auth else {
            return Ok(None);
        };
        if let Some(key) = &auth.setting {
            if let Some(v) = ctx.settings.get(key).await? {
                if !v.is_empty() {
                    return Ok(Some(v));
                }
            }
        }
        if let Some(var) = &auth.env {
            if let Ok(v) = std::env::var(var) {
                if !v.is_empty() {
                    return Ok(Some(v));
                }
            }
        }
        Ok(None)
    }

    /// Build the (sync) `Resolver` backing placeholder substitution.
    /// `SettingsStore::get` is async but `Resolver::setting` is not — rather
    /// than threading async through `ryuzi_plugin_sdk::subst::resolve` (a
    /// deliberately dependency-light, sync, already-tested routine), we
    /// pre-fetch every `${setting:KEY}` key referenced anywhere in this
    /// manifest's `[[mcp]]` entries up front and hand the sync resolver a
    /// plain map. `${auth}` is likewise resolved once up front;
    /// `${env:VAR}` needs no pre-fetch since `std::env::var` is sync.
    async fn resolver(&self, ctx: &ConnectorCtx) -> anyhow::Result<PreloadedResolver> {
        let auth = self.resolve_auth(ctx).await?;
        let mut settings = HashMap::new();
        for key in setting_keys(&self.manifest.mcp) {
            if let Some(v) = ctx.settings.get(&key).await? {
                settings.insert(key, v);
            }
        }
        Ok(PreloadedResolver { auth, settings })
    }
}

/// A `Resolver` backed by values fetched ahead of time (see
/// `DeclarativeConnector::resolver`'s doc for why).
struct PreloadedResolver {
    auth: Option<String>,
    settings: HashMap<String, String>,
}

impl Resolver for PreloadedResolver {
    fn auth(&self) -> Option<String> {
        self.auth.clone()
    }

    fn setting(&self, key: &str) -> Option<String> {
        self.settings.get(key).cloned()
    }

    fn env(&self, var: &str) -> Option<String> {
        std::env::var(var).ok()
    }
}

/// Every distinct `${setting:KEY}` key referenced in any of `servers`' args,
/// env values, header values, or url.
fn setting_keys(servers: &[McpServerDef]) -> HashSet<String> {
    let mut keys = HashSet::new();
    for server in servers {
        for s in server
            .args
            .iter()
            .chain(server.env.values())
            .chain(server.headers.values())
            .chain(server.url.iter())
        {
            collect_setting_keys(s, &mut keys);
        }
    }
    keys
}

/// Scan `s` for `${setting:KEY}` occurrences and insert each `KEY` into
/// `keys`. Deliberately not the full `subst` grammar (no escape handling) —
/// it only needs to find candidate keys to pre-fetch; `subst::resolve` does
/// the real, authoritative substitution (and rejects anything malformed)
/// afterward.
fn collect_setting_keys(s: &str, keys: &mut HashSet<String>) {
    let mut rest = s;
    while let Some(idx) = rest.find("${setting:") {
        let after = &rest[idx + "${setting:".len()..];
        match after.find('}') {
            Some(end) => {
                keys.insert(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
}

/// Map one `McpServerDef` to its live `McpServerSpec`, substituting
/// placeholders into args, env values, header values, and url.
fn build_spec(server: &McpServerDef, resolver: &dyn Resolver) -> anyhow::Result<McpServerSpec> {
    let transport = match server.transport {
        McpTransportDef::Stdio => {
            let command = server
                .command
                .clone()
                .ok_or_else(|| anyhow::anyhow!("mcp server \"{}\" has no command", server.name))?;
            let args = server
                .args
                .iter()
                .map(|a| resolve(a, resolver))
                .collect::<Result<Vec<_>, _>>()?;
            let mut env = Vec::with_capacity(server.env.len());
            for (k, v) in &server.env {
                env.push((k.clone(), resolve(v, resolver)?));
            }
            McpTransport::Stdio { command, args, env }
        }
        McpTransportDef::Http => {
            let raw_url = server
                .url
                .as_deref()
                .ok_or_else(|| anyhow::anyhow!("mcp server \"{}\" has no url", server.name))?;
            let url = resolve(raw_url, resolver)?;
            let mut headers = Vec::with_capacity(server.headers.len());
            for (k, v) in &server.headers {
                headers.push((k.clone(), resolve(v, resolver)?));
            }
            McpTransport::Http { url, headers }
        }
    };
    Ok(McpServerSpec {
        name: server.name.clone(),
        transport,
    })
}

#[cfg(test)]
mod tests {
    use super::super::host::PluginSource;
    use super::*;
    use crate::connector::ConnectorCtx;
    use crate::domain::McpTransport;
    use crate::settings::SettingsStore;
    use crate::store::Store;
    use ryuzi_plugin_sdk::PluginManifest;
    use std::sync::Arc;

    async fn open_settings() -> (Arc<Store>, SettingsStore, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(Store::open(tmp.path()).await.unwrap());
        let settings = SettingsStore::new(store.clone());
        (store, settings, tmp)
    }

    fn ctx(settings: SettingsStore) -> ConnectorCtx {
        ConnectorCtx {
            project_id: "p1".to_string(),
            work_dir: std::env::temp_dir(),
            settings,
        }
    }

    const GITHUB_STDIO_MANIFEST: &str = r#"
contract = 1
id = "github"
name = "GitHub"

[auth]
kind = "token"
setting = "plugin.github.token"
help_url = "https://github.com/settings/tokens"

[[mcp]]
name = "github"
transport = "stdio"
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]
env = { GITHUB_TOKEN = "${auth}" }
"#;

    #[tokio::test]
    async fn mcp_servers_resolves_auth_placeholder_from_settings_row() {
        let (store, settings, _tmp) = open_settings().await;
        store
            .set_setting_raw("plugin.github.token", "secret-token")
            .await
            .unwrap();

        let manifest = PluginManifest::from_toml(GITHUB_STDIO_MANIFEST).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin
            .connector
            .clone()
            .expect("an mcp-bearing manifest gets a connector");

        let servers = connector.mcp_servers(&ctx(settings)).await.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "github");
        match &servers[0].transport {
            McpTransport::Stdio { command, args, env } => {
                assert_eq!(command, "npx");
                assert_eq!(
                    args,
                    &vec![
                        "-y".to_string(),
                        "@modelcontextprotocol/server-github".to_string()
                    ]
                );
                assert_eq!(
                    env,
                    &vec![("GITHUB_TOKEN".to_string(), "secret-token".to_string())]
                );
            }
            other => panic!("expected stdio transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn ensure_auth_errs_with_help_url_when_secret_is_missing_everywhere() {
        let (_store, settings, _tmp) = open_settings().await;
        let manifest = PluginManifest::from_toml(GITHUB_STDIO_MANIFEST).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        let err = connector.ensure_auth(&ctx(settings)).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("github"),
            "message should name the plugin id: {msg}"
        );
        assert!(
            msg.contains("https://github.com/settings/tokens"),
            "message should include help_url: {msg}"
        );
    }

    #[tokio::test]
    async fn ensure_auth_is_ok_once_the_settings_row_is_present() {
        let (store, settings, _tmp) = open_settings().await;
        store
            .set_setting_raw("plugin.github.token", "secret-token")
            .await
            .unwrap();
        let manifest = PluginManifest::from_toml(GITHUB_STDIO_MANIFEST).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        connector.ensure_auth(&ctx(settings)).await.unwrap();
    }

    const ACME_REQUIRED_SETTING_MANIFEST: &str = r#"
contract = 1
id = "acme-required"
name = "Acme Required"

[auth]
kind = "token"
setting = "plugin.acme-required.token"
help_url = "https://acme.example.com/tokens"

[[settings]]
key = "plugin.acme-required.user"
label = "Acme user"
help = "Sent as a header identifying the acting user."
required = true

[[mcp]]
name = "acme-required"
transport = "http"
url = "https://mcp.acme.example.com"
headers = { Authorization = "Bearer ${auth}", X-Acme-User = "${setting:plugin.acme-required.user}" }
"#;

    #[tokio::test]
    async fn ensure_auth_errs_naming_the_missing_required_setting_field() {
        let (store, settings, _tmp) = open_settings().await;
        // Auth is satisfied — only the required non-auth setting is missing.
        store
            .set_setting_raw("plugin.acme-required.token", "secret-token")
            .await
            .unwrap();
        let manifest = PluginManifest::from_toml(ACME_REQUIRED_SETTING_MANIFEST).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        let err = connector.ensure_auth(&ctx(settings)).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("acme-required"),
            "message should name the plugin id: {msg}"
        );
        assert!(
            msg.contains("plugin.acme-required.user"),
            "message should name the missing required key: {msg}"
        );
        assert!(
            msg.contains("https://acme.example.com/tokens"),
            "message should include help_url: {msg}"
        );
    }

    #[tokio::test]
    async fn ensure_auth_is_ok_once_auth_and_required_settings_are_both_present() {
        let (store, settings, _tmp) = open_settings().await;
        store
            .set_setting_raw("plugin.acme-required.token", "secret-token")
            .await
            .unwrap();
        store
            .set_setting_raw("plugin.acme-required.user", "alice")
            .await
            .unwrap();
        let manifest = PluginManifest::from_toml(ACME_REQUIRED_SETTING_MANIFEST).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        connector.ensure_auth(&ctx(settings)).await.unwrap();
    }

    #[tokio::test]
    async fn env_placeholder_resolves_from_process_env_var() {
        let (_store, settings, _tmp) = open_settings().await;
        // Unique per-test var name: `cargo test` runs multi-threaded within a
        // crate, and `std::env` is process-global.
        let var = "RYUZI_TEST_DECLARATIVE_ENV_PLACEHOLDER_7f3c1a";
        std::env::set_var(var, "env-value");

        let toml_str = format!(
            r#"
contract = 1
id = "acme"
name = "Acme"

[[mcp]]
name = "svc"
transport = "stdio"
command = "acme-mcp"
env = {{ TOKEN = "${{env:{var}}}" }}
"#
        );
        let manifest = PluginManifest::from_toml(&toml_str).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        let servers = connector.mcp_servers(&ctx(settings)).await.unwrap();
        match &servers[0].transport {
            McpTransport::Stdio { env, .. } => {
                assert_eq!(env, &vec![("TOKEN".to_string(), "env-value".to_string())]);
            }
            other => panic!("expected stdio transport, got {other:?}"),
        }

        std::env::remove_var(var);
    }

    #[tokio::test]
    async fn setting_placeholder_resolves_from_a_non_auth_settings_row() {
        let (store, settings, _tmp) = open_settings().await;
        store
            .set_setting_raw("plugin.acme.host", "acme.example.com")
            .await
            .unwrap();

        let toml_str = r#"
contract = 1
id = "acme"
name = "Acme"

[[mcp]]
name = "svc"
transport = "stdio"
command = "acme-mcp"
args = ["--host", "${setting:plugin.acme.host}"]
"#;
        let manifest = PluginManifest::from_toml(toml_str).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        let servers = connector.mcp_servers(&ctx(settings)).await.unwrap();
        match &servers[0].transport {
            McpTransport::Stdio { args, .. } => {
                assert_eq!(
                    args,
                    &vec!["--host".to_string(), "acme.example.com".to_string()]
                );
            }
            other => panic!("expected stdio transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_manifest_resolves_substituted_headers() {
        let (store, settings, _tmp) = open_settings().await;
        store
            .set_setting_raw("plugin.acme-http.token", "tok-123")
            .await
            .unwrap();

        let toml_str = r#"
contract = 1
id = "acme-http"
name = "Acme HTTP"

[auth]
kind = "token"
setting = "plugin.acme-http.token"

[[mcp]]
name = "svc"
transport = "http"
url = "https://api.acme.dev/mcp"
headers = { Authorization = "Bearer ${auth}" }
"#;
        let manifest = PluginManifest::from_toml(toml_str).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        let servers = connector.mcp_servers(&ctx(settings)).await.unwrap();
        assert_eq!(servers.len(), 1);
        match &servers[0].transport {
            McpTransport::Http { url, headers } => {
                assert_eq!(url, "https://api.acme.dev/mcp");
                assert_eq!(
                    headers,
                    &vec![("Authorization".to_string(), "Bearer tok-123".to_string())]
                );
            }
            other => panic!("expected http transport, got {other:?}"),
        }
    }

    #[test]
    fn manifest_with_no_mcp_servers_gets_no_connector() {
        let toml_str = r#"
contract = 1
id = "meta-only"
name = "Meta Only"
"#;
        let manifest = PluginManifest::from_toml(toml_str).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        assert!(plugin.connector.is_none());
        assert!(plugin.harness.is_none());
        assert!(plugin.gateway.is_none());
    }

    #[test]
    fn declarative_plugin_rejects_a_structurally_invalid_manifest() {
        // Hand-built (not through `from_toml`, which would already validate),
        // so this exercises `declarative_plugin`'s own `validate()` call.
        let manifest = PluginManifest {
            contract: 1,
            id: "bad id with spaces".to_string(),
            name: "Bad".to_string(),
            version: String::new(),
            publisher: String::new(),
            description: String::new(),
            homepage: None,
            icon: None,
            categories: vec![],
            verified: false,
            experimental: false,
            auth: None,
            settings: vec![],
            mcp: vec![],
            skills: vec![],
            menu: None,
            provider: None,
            runtime: None,
        };
        assert!(declarative_plugin(manifest, PluginSource::Catalog).is_err());
    }
}
