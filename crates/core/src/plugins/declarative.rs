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
use serde::Deserialize;

use crate::connector::{Connector, ConnectorCtx};
use crate::domain::{McpServerSpec, McpTransport};
use crate::plugins::oauth::{needs_refresh, PluginOauthToken};

use super::host::{CorePlugin, PluginSource};

const TERMINAL_OAUTH_REFRESH_ERRORS: &[&str] =
    &["invalid_grant", "refresh_token_reused", "invalid_request"];

#[derive(Debug, Deserialize)]
struct PluginOauthRefreshResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

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
            if auth.kind == AuthKind::Oauth && self.uses_http_oauth() {
                self.resolve_http_oauth_bearer_token(ctx).await?;
            } else if auth.kind != AuthKind::None && self.resolve_auth(ctx).await?.is_none() {
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
        let oauth_bearer_token = self.resolve_http_oauth_bearer_token(ctx).await?;
        let mut settings = HashMap::new();
        for key in setting_keys(&self.manifest.mcp) {
            if let Some(v) = ctx.settings.get(&key).await? {
                settings.insert(key, v);
            }
        }
        Ok(PreloadedResolver {
            auth,
            oauth_bearer_token,
            settings,
        })
    }

    fn uses_http_oauth(&self) -> bool {
        self.manifest
            .auth
            .as_ref()
            .is_some_and(|auth| auth.kind == AuthKind::Oauth)
            && self
                .manifest
                .mcp
                .iter()
                .any(|server| server.transport == McpTransportDef::Http)
    }

    async fn resolve_http_oauth_bearer_token(
        &self,
        ctx: &ConnectorCtx,
    ) -> anyhow::Result<Option<String>> {
        if !self.uses_http_oauth() {
            return Ok(None);
        }
        let id = &self.manifest.id;
        let Some(token) = ctx.settings.store().get_plugin_oauth_token(id).await? else {
            return Err(self.http_oauth_auth_required_error(false));
        };
        if token.reconnect_required {
            return Err(self.http_oauth_auth_required_error(true));
        }
        if !needs_refresh(crate::paths::now_ms(), token.expires_at) {
            return Ok(Some(token.access_token));
        }
        let refreshed = self.refresh_http_oauth_token(ctx, token).await?;
        Ok(Some(refreshed.access_token))
    }

    async fn refresh_http_oauth_token(
        &self,
        ctx: &ConnectorCtx,
        token: PluginOauthToken,
    ) -> anyhow::Result<PluginOauthToken> {
        let id = &self.manifest.id;
        let store = ctx.settings.store();
        let Some(auth) = self.manifest.auth.as_ref() else {
            return Ok(token);
        };
        let Some(refresh_token) = token
            .refresh_token
            .as_deref()
            .filter(|value| !value.is_empty())
        else {
            store.mark_plugin_oauth_reconnect_required(id).await?;
            return Err(self.http_oauth_auth_required_error(true));
        };
        let Some(token_url) = auth.token_url.as_deref().filter(|value| !value.is_empty()) else {
            store.mark_plugin_oauth_reconnect_required(id).await?;
            return Err(self.http_oauth_auth_required_error(true));
        };

        let mut form = vec![
            ("grant_type".to_string(), "refresh_token".to_string()),
            ("refresh_token".to_string(), refresh_token.to_string()),
        ];
        self.push_refresh_setting(
            &mut form,
            ctx,
            auth.client_id_setting.as_deref(),
            "client_id",
        )
        .await?;
        self.push_refresh_setting(
            &mut form,
            ctx,
            auth.client_secret_setting.as_deref(),
            "client_secret",
        )
        .await?;
        if let Some(resource) = auth.resource.as_deref().filter(|value| !value.is_empty()) {
            form.push(("resource".to_string(), resource.to_string()));
        }
        for (key, value) in &auth.extra_token_params {
            form.push((key.clone(), value.clone()));
        }

        let http = reqwest::Client::new();
        let response = http.post(token_url).form(&form).send().await?;
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        if !status.is_success() {
            if is_terminal_oauth_refresh_error(&body) {
                store.mark_plugin_oauth_reconnect_required(id).await?;
                return Err(self.http_oauth_auth_required_error(true));
            }
            let detail = body.trim();
            if detail.is_empty() {
                anyhow::bail!("{id} OAuth token refresh failed with HTTP {status}");
            }
            anyhow::bail!("{id} OAuth token refresh failed with HTTP {status}: {detail}");
        }

        let payload: PluginOauthRefreshResponse = serde_json::from_str(&body)?;
        let access_token = payload
            .access_token
            .filter(|token| !token.is_empty())
            .ok_or_else(|| {
                anyhow::anyhow!("{id} OAuth token refresh response is missing access_token")
            })?;
        let scopes = payload
            .scope
            .map(|scope| {
                scope
                    .split_whitespace()
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .collect::<Vec<_>>()
            })
            .filter(|scopes| !scopes.is_empty())
            .unwrap_or_else(|| token.scopes.clone());
        let refreshed = PluginOauthToken {
            plugin_id: token.plugin_id.clone(),
            access_token,
            refresh_token: payload
                .refresh_token
                .filter(|refresh_token| !refresh_token.is_empty())
                .or(token.refresh_token.clone()),
            token_type: payload
                .token_type
                .filter(|token_type| !token_type.is_empty())
                .unwrap_or_else(|| token.token_type.clone()),
            expires_at: payload
                .expires_in
                .map(|seconds| crate::paths::now_ms() + seconds.saturating_mul(1000)),
            scopes,
            reconnect_required: false,
        };
        store.upsert_plugin_oauth_token(&refreshed).await?;
        Ok(refreshed)
    }

    async fn push_refresh_setting(
        &self,
        form: &mut Vec<(String, String)>,
        ctx: &ConnectorCtx,
        key: Option<&str>,
        form_key: &str,
    ) -> anyhow::Result<()> {
        let Some(key) = key else {
            return Ok(());
        };
        match ctx.settings.get(key).await? {
            Some(value) if !value.is_empty() => form.push((form_key.to_string(), value)),
            _ => {
                ctx.settings
                    .store()
                    .mark_plugin_oauth_reconnect_required(&self.manifest.id)
                    .await?;
                return Err(self.http_oauth_auth_required_error(true));
            }
        }
        Ok(())
    }

    fn http_oauth_auth_required_error(&self, reconnect_required: bool) -> anyhow::Error {
        let id = &self.manifest.id;
        let help_url = self
            .manifest
            .auth
            .as_ref()
            .and_then(|auth| auth.help_url.as_deref());
        match (reconnect_required, help_url) {
            (true, Some(url)) => {
                anyhow::anyhow!("configure {id}: reconnect OAuth access — see {url}")
            }
            (true, None) => anyhow::anyhow!("configure {id}: reconnect OAuth access"),
            (false, Some(url)) => {
                anyhow::anyhow!("configure {id}: OAuth login required — see {url}")
            }
            (false, None) => anyhow::anyhow!("configure {id}: OAuth login required"),
        }
    }
}

fn is_terminal_oauth_refresh_error(body: &str) -> bool {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return false;
    };
    matches!(
        json.get("error").and_then(serde_json::Value::as_str),
        Some(code) if TERMINAL_OAUTH_REFRESH_ERRORS.contains(&code)
    )
}

/// A `Resolver` backed by values fetched ahead of time (see
/// `DeclarativeConnector::resolver`'s doc for why).
struct PreloadedResolver {
    auth: Option<String>,
    oauth_bearer_token: Option<String>,
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
fn build_spec(
    server: &McpServerDef,
    resolver: &PreloadedResolver,
) -> anyhow::Result<McpServerSpec> {
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
            if let Some(token) = resolver.oauth_bearer_token.as_deref() {
                let bearer = format!("Bearer {token}");
                match headers
                    .iter_mut()
                    .find(|(key, _)| key.eq_ignore_ascii_case("Authorization"))
                {
                    Some((_, value)) => *value = bearer,
                    None => headers.push(("Authorization".to_string(), bearer)),
                }
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
    use crate::llm_router::secrets::use_test_key_file;
    use crate::plugins::oauth::PluginOauthToken;
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

    const ACME_HTTP_OAUTH_MANIFEST: &str = r#"
contract = 1
id = "acme-http-oauth"
name = "Acme HTTP OAuth"

[auth]
kind = "oauth"
help_url = "https://acme.example.com/oauth"

[[mcp]]
name = "svc"
transport = "http"
url = "https://api.acme.dev/mcp"
headers = { Authorization = "Basic stale", X-Trace = "trace-123" }
"#;

    const GOOGLE_STYLE_STDIO_OAUTH_MANIFEST: &str = r#"
contract = 1
id = "google-workspace"
name = "Google Workspace"

[auth]
kind = "oauth"
setting = "plugin.google-workspace.client_id"
env = "GOOGLE_OAUTH_CLIENT_ID"
help_url = "https://github.com/taylorwilsdon/google_workspace_mcp"

[[mcp]]
name = "google-workspace"
transport = "stdio"
command = "uvx"
args = ["workspace-mcp"]
env = { GOOGLE_OAUTH_CLIENT_ID = "${auth}" }
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

    #[tokio::test]
    async fn http_oauth_manifest_injects_bearer_token_from_stored_plugin_oauth_token() {
        use_test_key_file();
        let (store, settings, _tmp) = open_settings().await;
        store
            .upsert_plugin_oauth_token(&PluginOauthToken {
                plugin_id: "acme-http-oauth".into(),
                access_token: "oauth-secret".into(),
                refresh_token: None,
                token_type: "Bearer".into(),
                expires_at: Some(crate::paths::now_ms() + 60 * 60 * 1000),
                scopes: vec![],
                reconnect_required: false,
            })
            .await
            .unwrap();

        let manifest = PluginManifest::from_toml(ACME_HTTP_OAUTH_MANIFEST).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        let servers = connector.mcp_servers(&ctx(settings)).await.unwrap();
        assert_eq!(servers.len(), 1);
        match &servers[0].transport {
            McpTransport::Http { url, headers } => {
                let header_map: HashMap<String, String> = headers.iter().cloned().collect();
                assert_eq!(url, "https://api.acme.dev/mcp");
                assert_eq!(
                    header_map.get("Authorization"),
                    Some(&"Bearer oauth-secret".to_string())
                );
                assert_eq!(header_map.get("X-Trace"), Some(&"trace-123".to_string()));
                assert_eq!(header_map.len(), 2);
            }
            other => panic!("expected http transport, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn http_oauth_manifest_refreshes_expired_token_before_injecting_bearer() {
        use axum::{extract::Form, routing::post, Json, Router};
        use serde_json::json;

        use_test_key_file();
        let app = Router::new().route(
            "/token",
            post(|Form(form): Form<HashMap<String, String>>| async move {
                assert_eq!(
                    form.get("grant_type").map(String::as_str),
                    Some("refresh_token")
                );
                assert_eq!(
                    form.get("refresh_token").map(String::as_str),
                    Some("refresh-old")
                );
                Json(json!({
                    "access_token": "oauth-new",
                    "refresh_token": "refresh-new",
                    "token_type": "Bearer",
                    "expires_in": 3600,
                    "scope": "read write"
                }))
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token_url = format!("http://{}/token", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let (store, settings, _tmp) = open_settings().await;
        store
            .upsert_plugin_oauth_token(&PluginOauthToken {
                plugin_id: "acme-http-oauth".into(),
                access_token: "oauth-old".into(),
                refresh_token: Some("refresh-old".into()),
                token_type: "Bearer".into(),
                expires_at: Some(crate::paths::now_ms() - 1),
                scopes: vec!["read".into()],
                reconnect_required: false,
            })
            .await
            .unwrap();

        let toml = ACME_HTTP_OAUTH_MANIFEST.replace(
            "help_url = \"https://acme.example.com/oauth\"",
            &format!("help_url = \"https://acme.example.com/oauth\"\ntoken-url = \"{token_url}\""),
        );
        let manifest = PluginManifest::from_toml(&toml).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        let servers = connector.mcp_servers(&ctx(settings)).await.unwrap();
        match &servers[0].transport {
            McpTransport::Http { headers, .. } => {
                let header_map: HashMap<String, String> = headers.iter().cloned().collect();
                assert_eq!(
                    header_map.get("Authorization"),
                    Some(&"Bearer oauth-new".to_string())
                );
            }
            other => panic!("expected http transport, got {other:?}"),
        }

        let refreshed = store
            .get_plugin_oauth_token("acme-http-oauth")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(refreshed.access_token, "oauth-new");
        assert_eq!(refreshed.refresh_token.as_deref(), Some("refresh-new"));
        assert_eq!(
            refreshed.scopes,
            vec!["read".to_string(), "write".to_string()]
        );
        assert!(!refreshed.reconnect_required);
        assert!(refreshed.expires_at.unwrap() > crate::paths::now_ms());
    }

    #[tokio::test]
    async fn http_oauth_manifest_marks_expired_token_without_refresh_as_reconnect_required() {
        use_test_key_file();
        let (store, settings, _tmp) = open_settings().await;
        store
            .upsert_plugin_oauth_token(&PluginOauthToken {
                plugin_id: "acme-http-oauth".into(),
                access_token: "oauth-old".into(),
                refresh_token: None,
                token_type: "Bearer".into(),
                expires_at: Some(crate::paths::now_ms() - 1),
                scopes: vec![],
                reconnect_required: false,
            })
            .await
            .unwrap();
        let manifest = PluginManifest::from_toml(ACME_HTTP_OAUTH_MANIFEST).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        let err = connector.mcp_servers(&ctx(settings)).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("reconnect"),
            "message should explain that the plugin needs reconnecting: {msg}"
        );
        let stale = store
            .get_plugin_oauth_token("acme-http-oauth")
            .await
            .unwrap()
            .unwrap();
        assert!(stale.reconnect_required);
    }

    #[tokio::test]
    async fn http_oauth_manifest_marks_terminal_refresh_error_as_reconnect_required() {
        use axum::{http::StatusCode, response::IntoResponse, routing::post, Json, Router};
        use serde_json::json;

        use_test_key_file();
        let app = Router::new().route(
            "/token",
            post(|| async {
                (
                    StatusCode::BAD_REQUEST,
                    Json(json!({ "error": "invalid_grant" })),
                )
                    .into_response()
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let token_url = format!("http://{}/token", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let (store, settings, _tmp) = open_settings().await;
        store
            .upsert_plugin_oauth_token(&PluginOauthToken {
                plugin_id: "acme-http-oauth".into(),
                access_token: "oauth-old".into(),
                refresh_token: Some("refresh-old".into()),
                token_type: "Bearer".into(),
                expires_at: Some(crate::paths::now_ms() - 1),
                scopes: vec![],
                reconnect_required: false,
            })
            .await
            .unwrap();

        let toml = ACME_HTTP_OAUTH_MANIFEST.replace(
            "help_url = \"https://acme.example.com/oauth\"",
            &format!("help_url = \"https://acme.example.com/oauth\"\ntoken-url = \"{token_url}\""),
        );
        let manifest = PluginManifest::from_toml(&toml).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        let err = connector.ensure_auth(&ctx(settings)).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("reconnect"),
            "message should explain that the plugin needs reconnecting: {msg}"
        );
        let stale = store
            .get_plugin_oauth_token("acme-http-oauth")
            .await
            .unwrap()
            .unwrap();
        assert!(stale.reconnect_required);
    }

    #[tokio::test]
    async fn http_oauth_manifest_requires_stored_plugin_token_for_ensure_auth_and_mcp_servers() {
        let (_store, settings, _tmp) = open_settings().await;
        let manifest = PluginManifest::from_toml(ACME_HTTP_OAUTH_MANIFEST).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        let err = connector
            .ensure_auth(&ctx(settings.clone()))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("configure acme-http-oauth"),
            "message should name the plugin id: {msg}"
        );
        assert!(
            msg.contains("https://acme.example.com/oauth"),
            "message should include help_url: {msg}"
        );

        let err = connector.mcp_servers(&ctx(settings)).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("configure acme-http-oauth"),
            "message should name the plugin id: {msg}"
        );
    }

    #[tokio::test]
    async fn http_oauth_manifest_requires_reconnect_when_stored_plugin_token_is_marked_stale() {
        use_test_key_file();
        let (store, settings, _tmp) = open_settings().await;
        store
            .upsert_plugin_oauth_token(&PluginOauthToken {
                plugin_id: "acme-http-oauth".into(),
                access_token: "oauth-secret".into(),
                refresh_token: Some("refresh-secret".into()),
                token_type: "Bearer".into(),
                expires_at: None,
                scopes: vec![],
                reconnect_required: true,
            })
            .await
            .unwrap();
        let manifest = PluginManifest::from_toml(ACME_HTTP_OAUTH_MANIFEST).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        let err = connector.ensure_auth(&ctx(settings)).await.unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("configure acme-http-oauth"),
            "message should name the plugin id: {msg}"
        );
        assert!(
            msg.contains("reconnect"),
            "message should explain that the plugin needs reconnecting: {msg}"
        );
    }

    #[tokio::test]
    async fn stdio_oauth_manifest_still_resolves_auth_placeholder_from_setting() {
        let (store, settings, _tmp) = open_settings().await;
        store
            .set_setting_raw("plugin.google-workspace.client_id", "client-id-123")
            .await
            .unwrap();

        let manifest = PluginManifest::from_toml(GOOGLE_STYLE_STDIO_OAUTH_MANIFEST).unwrap();
        let plugin = declarative_plugin(manifest, PluginSource::Catalog).unwrap();
        let connector = plugin.connector.clone().unwrap();

        connector.ensure_auth(&ctx(settings.clone())).await.unwrap();
        let servers = connector.mcp_servers(&ctx(settings)).await.unwrap();
        match &servers[0].transport {
            McpTransport::Stdio { env, .. } => {
                assert_eq!(
                    env,
                    &vec![(
                        "GOOGLE_OAUTH_CLIENT_ID".to_string(),
                        "client-id-123".to_string()
                    )]
                );
            }
            other => panic!("expected stdio transport, got {other:?}"),
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
