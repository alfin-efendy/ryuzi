//! Plugins screen commands: every installed plugin's identity/capabilities
//! (`list_plugins`), a single plugin's full detail (`plugin_detail`),
//! enable/disable (`set_plugin_enabled` — delegates to
//! [`ryuzi_core::plugins::toggle_enabled`], the same helper `ryuzi plugins
//! enable/disable` uses, so the two surfaces can never drift), a validated
//! settings write (`set_plugin_setting`), and a provider's effective model
//! list (`plugin_models`).
//!
//! DTOs here are deliberate thin mirrors of `ryuzi_plugin_sdk::PluginManifest`
//! (and `ryuzi_core::plugins::CorePlugin`) rather than re-exports: the
//! manifest is the engine's contract for plugin authors, while these shapes
//! are the Cockpit UI's contract, free to add UI-only fields (like
//! `value_set`/`configured` booleans) without perturbing the engine type.
//!
//! Secrets are never returned: `PluginAuthInfo.configured` and
//! `PluginFieldInfo.value_set` are booleans derived from whether a row is
//! persisted (or an auth env var is set), never the value itself.

use crate::error::CmdError;
use crate::events::{PluginOauthAuthorizeUrlMsg, PluginOauthCompletedMsg};
use reqwest::Url;
use ryuzi_core::plugins::doctor;
use ryuzi_core::plugins::oauth::{
    discover_oauth_server_metadata, generate_pkce_verifier, pkce_challenge_s256,
    register_oauth_client, OauthServerMetadata, PluginOauthToken,
};
use ryuzi_core::plugins::providers;
use ryuzi_core::settings::SettingsStore;
use ryuzi_core::skills_install::{BeginInstall, InstalledSkillPack, TrustPrompt, UpdateOutcome};
use ryuzi_core::store::PluginOauthClient;
use ryuzi_core::{ControlPlane, CorePlugin, PluginSource, Store};
use ryuzi_plugin_sdk::{AuthKind, AuthSpec, McpServerDef, McpTransportDef, SettingField};
use serde::{Deserialize, Serialize};
use specta::Type;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, State};
use tauri_plugin_opener::OpenerExt;
use tauri_specta::Event as _;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub description: String,
    pub icon: Option<String>,
    pub categories: Vec<String>,
    pub verified: bool,
    pub experimental: bool,
    pub enabled: bool,
    /// Same semantics as `PluginAuthInfo.configured` (oauth: token stored &&
    /// !reconnect_required; else a persisted `auth.setting` row or `auth.env`
    /// set). `false` when the manifest declares no `[auth]` block. On the
    /// LIST payload (not just `plugin_detail`) because the Browse grid's
    /// Install/Open split needs it — note this adds per-plugin store lookups
    /// to list assembly.
    pub configured: bool,
    /// `builtin` | `catalog` | `skill-pack`.
    pub source: String,
    /// Any of `provider` | `runtime` | `gateway` | `connector`.
    pub capabilities: Vec<String>,
    /// `integration` | `provider` | `gateway` | `skill-pack`. Runtime-kind
    /// plugins are excluded from the list — the Runtime page owns them.
    pub kind: String,
    /// Kind-specific "already set up" flag: integration = configured ||
    /// enabled; provider = ≥1 connection in the provider's family; gateway =
    /// all manifest settings present; skill-pack = installed on disk.
    pub installed: bool,
    /// Provider family head id (providers only) — the Models `providerDetail`
    /// navigation target. `None` for other kinds.
    pub family: Option<String>,
    /// Mirrors `crate::store::PluginInstallRecord.pinned` — `false` when the
    /// plugin has no `plugin_installs` ledger row (never installed via the
    /// tracked git-clone path, e.g. builtins/catalog integrations with no
    /// skill-pack install).
    pub pinned: bool,
    /// The ledger row's git origin (`PluginInstallRecord.source_spec`).
    /// Distinct from `source` (the stable builtin/catalog/skill-pack enum
    /// label) — the daemon's `/plugins` route (`serve::merge_install_record`)
    /// draws the same distinction.
    pub source_spec: Option<String>,
    pub resolved_commit: Option<String>,
    pub installed_at: Option<i64>,
    pub updated_at: Option<i64>,
    pub trust_tier: Option<String>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginAuthInfo {
    /// `none` | `api-key` | `token` | `oauth`.
    pub kind: String,
    pub setting: Option<String>,
    pub env: Option<String>,
    pub help_url: Option<String>,
    /// A persisted (non-empty) row exists for `setting`, OR `env` is set in
    /// the process environment. Never reveals the value itself.
    pub configured: bool,
    pub oauth_connect_available: bool,
    pub oauth_connect_error: Option<String>,
    pub oauth_token_stored: bool,
    pub oauth_reconnect_required: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginOauthBeginResult {
    pub state_token: String,
    pub authorize_url: String,
    pub redirect_uri: String,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginInstallBeginResult {
    /// `none` | `api-key` | `token` | `oauth`.
    pub auth_kind: String,
    /// `auth.env` is declared AND set in the environment.
    pub env_var_present: bool,
    pub env_var_name: Option<String>,
    /// Endpoints + client id resolved; the browser flow started.
    pub oauth_available: bool,
    /// OAuth brokered outside Cockpit (kind=oauth, no `auth.resource`, no
    /// manifest `authorize_url` — google-workspace).
    pub oauth_external: bool,
    /// oauth, endpoints may be known, but no client id and DCR not
    /// applicable / failed.
    pub needs_client_id: bool,
    /// This call performed a successful registration.
    pub dcr_succeeded: bool,
    /// `auto` (callback server bound) | `manual` (bind failed → paste).
    pub callback_mode: String,
    pub oauth_begin: Option<PluginOauthBeginResult>,
    /// Discovery/DCR failure detail (shown on the manual client id form).
    pub dcr_error: Option<String>,
}

impl PluginInstallBeginResult {
    fn new(auth_kind: &str) -> Self {
        Self {
            auth_kind: auth_kind.to_string(),
            env_var_present: false,
            env_var_name: None,
            oauth_available: false,
            oauth_external: false,
            needs_client_id: false,
            dcr_succeeded: false,
            callback_mode: "manual".to_string(),
            oauth_begin: None,
            dcr_error: None,
        }
    }
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginFieldInfo {
    pub key: String,
    pub label: String,
    pub help: String,
    pub secret: bool,
    pub required: bool,
    /// A persisted (non-empty) row exists for `key`. Never the value itself.
    pub value_set: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginMcpInfo {
    pub name: String,
    /// `stdio` | `http`.
    pub transport: String,
    /// The raw manifest string (command for stdio, url for http) — no
    /// `${auth}` substitution, matching `ryuzi plugins info`'s output.
    pub command_or_url: String,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct PluginDetail {
    pub info: PluginInfo,
    pub auth: Option<PluginAuthInfo>,
    pub settings: Vec<PluginFieldInfo>,
    pub mcp: Vec<PluginMcpInfo>,
    pub models: Vec<String>,
    pub homepage: Option<String>,
    pub publisher: String,
}

#[derive(Clone)]
struct PluginOauthFlowState {
    verifier: String,
    redirect_uri: String,
    requested_scopes: Vec<String>,
}

#[derive(Debug, Deserialize)]
struct PluginOauthTokenResponse {
    access_token: Option<String>,
    refresh_token: Option<String>,
    token_type: Option<String>,
    expires_in: Option<i64>,
    scope: Option<String>,
}

static PLUGIN_OAUTH_FLOWS: OnceLock<Mutex<HashMap<String, PluginOauthFlowState>>> = OnceLock::new();

fn plugin_oauth_flows() -> &'static Mutex<HashMap<String, PluginOauthFlowState>> {
    PLUGIN_OAUTH_FLOWS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Cancellation handles for pending loopback callback servers, keyed by the
/// same `{plugin_id}:{state_token}` key as `PLUGIN_OAUTH_FLOWS`. Firing (or
/// dropping) one makes the background task exit without emitting a
/// completion event.
static PLUGIN_INSTALL_CANCELS: OnceLock<Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>>> =
    OnceLock::new();

fn plugin_install_cancels() -> &'static Mutex<HashMap<String, tokio::sync::oneshot::Sender<()>>> {
    PLUGIN_INSTALL_CANCELS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Drop pending flow state (and shut down any live callback server) for
/// `plugin_id` — all of its flows when `state_token` is `None`, else just
/// that one flow.
fn cancel_pending_plugin_flows(plugin_id: &str, state_token: Option<&str>) {
    let prefix = format!("{plugin_id}:");
    let keys: Vec<String> = {
        let mut flows = plugin_oauth_flows()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        match state_token {
            Some(token) => {
                let key = plugin_oauth_flow_key(plugin_id, token);
                flows.remove(&key);
                vec![key]
            }
            None => {
                let keys: Vec<String> = flows
                    .keys()
                    .filter(|k| k.starts_with(&prefix))
                    .cloned()
                    .collect();
                for key in &keys {
                    flows.remove(key);
                }
                keys
            }
        }
    };
    let mut cancels = plugin_install_cancels()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    // A cancel sender can exist for a flow already removed (and vice versa)
    // — sweep by prefix as well as by the exact keys.
    let cancel_keys: Vec<String> = cancels
        .keys()
        .filter(|k| keys.contains(k) || (state_token.is_none() && k.starts_with(&prefix)))
        .cloned()
        .collect();
    for key in cancel_keys {
        if let Some(tx) = cancels.remove(&key) {
            let _ = tx.send(());
        }
    }
}

fn source_label(source: &PluginSource) -> &'static str {
    match source {
        PluginSource::Builtin => "builtin",
        PluginSource::Catalog => "catalog",
        PluginSource::SkillPack(_) => "skill-pack",
    }
}

/// The catalog kind for a plugin, or `None` when it must not be listed
/// (runtimes). Order matters: a provider manifest wins over runtime meta
/// (ollama is both), and a skill-pack source wins over connector shape.
fn derive_kind(plugin: &CorePlugin) -> Option<&'static str> {
    if plugin.manifest.provider.is_some() {
        return Some("provider");
    }
    if plugin.harness.is_some() || plugin.manifest.runtime.is_some() {
        return None;
    }
    if matches!(plugin.source, PluginSource::SkillPack(_)) {
        return Some("skill-pack");
    }
    if plugin.gateway.is_some()
        || plugin
            .manifest
            .categories
            .iter()
            .any(|c| c == "chat-gateway")
    {
        return Some("gateway");
    }
    Some("integration")
}

/// Family head id for a provider plugin (`anthropic-oauth` → `anthropic`).
fn provider_family(id: &str) -> String {
    ryuzi_core::llm_router::registry::descriptor(id)
        .map(|d| d.family.to_string())
        .unwrap_or_else(|| id.to_string())
}

/// Pure kind → installed decision. Inputs are pre-computed by the caller.
fn installed_flag(
    kind: &str,
    enabled: bool,
    configured: bool,
    has_family_connection: bool,
    gateway_settings_complete: bool,
    skill_pack_installed: bool,
) -> bool {
    match kind {
        "provider" => has_family_connection,
        "gateway" => gateway_settings_complete,
        "skill-pack" => skill_pack_installed,
        _ => configured || enabled,
    }
}

/// Populate `PluginInfo`'s ledger-derived fields (`pinned`, `sourceSpec`,
/// `resolvedCommit`, `installedAt`, `updatedAt`, `trustTier`) from an
/// optional `plugin_installs` row — `None` leaves them at their "no ledger
/// row" defaults (`pinned: false`, the rest `None`).
struct InstallLedgerFields {
    pinned: bool,
    source_spec: Option<String>,
    resolved_commit: Option<String>,
    installed_at: Option<i64>,
    updated_at: Option<i64>,
    trust_tier: Option<String>,
}

impl InstallLedgerFields {
    fn absent() -> Self {
        Self {
            pinned: false,
            source_spec: None,
            resolved_commit: None,
            installed_at: None,
            updated_at: None,
            trust_tier: None,
        }
    }

    fn from_record(rec: &ryuzi_core::store::PluginInstallRecord) -> Self {
        Self {
            pinned: rec.pinned,
            source_spec: Some(rec.source_spec.clone()),
            resolved_commit: rec.resolved_commit.clone(),
            installed_at: Some(rec.installed_at),
            updated_at: Some(rec.updated_at),
            trust_tier: Some(rec.trust_tier.clone()),
        }
    }

    fn from_option(rec: Option<&ryuzi_core::store::PluginInstallRecord>) -> Self {
        rec.map(Self::from_record).unwrap_or_else(Self::absent)
    }
}

fn plugin_info(
    plugin: &CorePlugin,
    enabled: bool,
    configured: bool,
    kind: &str,
    installed: bool,
    install: Option<&ryuzi_core::store::PluginInstallRecord>,
) -> PluginInfo {
    let m = &plugin.manifest;
    let ledger = InstallLedgerFields::from_option(install);
    PluginInfo {
        id: m.id.clone(),
        name: m.name.clone(),
        description: m.description.clone(),
        icon: m.icon.clone(),
        categories: m.categories.clone(),
        verified: m.verified,
        experimental: m.experimental,
        enabled,
        configured,
        source: source_label(&plugin.source).to_string(),
        capabilities: plugin
            .capabilities()
            .into_iter()
            .map(str::to_string)
            .collect(),
        kind: kind.to_string(),
        installed,
        family: (kind == "provider").then(|| provider_family(&m.id)),
        pinned: ledger.pinned,
        source_spec: ledger.source_spec,
        resolved_commit: ledger.resolved_commit,
        installed_at: ledger.installed_at,
        updated_at: ledger.updated_at,
        trust_tier: ledger.trust_tier,
    }
}

fn auth_kind_label(kind: AuthKind) -> &'static str {
    match kind {
        AuthKind::None => "none",
        AuthKind::ApiKey => "api-key",
        AuthKind::Token => "token",
        AuthKind::Oauth => "oauth",
    }
}

fn plugin_oauth_flow_key(plugin_id: &str, state_token: &str) -> String {
    format!("{plugin_id}:{state_token}")
}

/// The wizard's loopback callback server port. Registered redirect URIs use
/// it, so it can never change without re-registering every DCR client.
const PLUGIN_OAUTH_CALLBACK_PORT: u16 = 8976;

fn plugin_oauth_callback_path(plugin_id: &str) -> String {
    format!("/plugin-oauth/{plugin_id}/callback")
}

fn plugin_oauth_redirect_uri(plugin_id: &str) -> String {
    format!(
        "http://127.0.0.1:{PLUGIN_OAUTH_CALLBACK_PORT}{}",
        plugin_oauth_callback_path(plugin_id)
    )
}

fn plugin_oauth_requested_scopes(auth: &AuthSpec) -> Vec<String> {
    auth.scopes.clone()
}

/// The effective OAuth config after the resolution order. Endpoints:
/// `plugin_oauth_clients` row (discovery/DCR/manual cache) → manifest.
/// Client id: row → saved value of the manifest's `auth.client_id_setting`
/// → for EXTERNAL OAuth plugins only, the saved `auth.setting` value
/// (google-workspace's client id key IS its auth.setting).
#[derive(Clone)]
struct ResolvedPluginOauth {
    authorize_url: Option<String>,
    token_url: Option<String>,
    client_id: Option<String>,
}

/// External OAuth: sign-in is brokered outside Cockpit by the child server —
/// kind=oauth with neither an `auth.resource` to discover against nor a
/// manifest `authorize_url` (google-workspace).
fn is_external_oauth(auth: &AuthSpec) -> bool {
    auth.kind == AuthKind::Oauth
        && auth.resource.as_deref().is_none_or(str::is_empty)
        && auth.authorize_url.as_deref().is_none_or(str::is_empty)
}

async fn resolve_plugin_oauth(
    store: &Store,
    plugin_id: &str,
    auth: &AuthSpec,
) -> anyhow::Result<ResolvedPluginOauth> {
    let row = store.get_plugin_oauth_client(plugin_id).await?;
    let (row_authorize, row_token, row_client) = match row {
        Some(row) => (row.authorize_url, row.token_url, row.client_id),
        None => (None, None, None),
    };
    let non_empty = |value: Option<String>| value.filter(|v| !v.is_empty());
    let authorize_url =
        non_empty(row_authorize).or_else(|| auth.authorize_url.clone().filter(|v| !v.is_empty()));
    let token_url =
        non_empty(row_token).or_else(|| auth.token_url.clone().filter(|v| !v.is_empty()));
    let mut client_id = non_empty(row_client);
    if client_id.is_none() {
        if let Some(key) = auth.client_id_setting.as_deref() {
            client_id = store.get_setting_raw(key).await?.filter(|v| !v.is_empty());
        }
    }
    if client_id.is_none() && is_external_oauth(auth) {
        if let Some(key) = auth.setting.as_deref() {
            client_id = store.get_setting_raw(key).await?.filter(|v| !v.is_empty());
        }
    }
    Ok(ResolvedPluginOauth {
        authorize_url,
        token_url,
        client_id,
    })
}

/// Prereq check over RESOLVED values (table already consulted). Two client-id
/// message variants preserved: missing `auth.client_id_setting` declaration
/// vs missing "saved value for {key}" — the wizard branches on structured
/// fields, never on this text.
fn plugin_oauth_prereq_error(
    plugin_id: &str,
    auth: &AuthSpec,
    resolved: &ResolvedPluginOauth,
) -> Option<String> {
    let mut missing = Vec::new();
    if resolved.authorize_url.is_none() {
        missing.push("auth.authorize_url".to_string());
    }
    if resolved.token_url.is_none() {
        missing.push("auth.token_url".to_string());
    }
    if resolved.client_id.is_none() {
        match auth.client_id_setting.as_deref() {
            Some(key) => missing.push(format!("saved value for {key}")),
            None => missing.push("auth.client_id_setting".to_string()),
        }
    }
    if missing.is_empty() {
        None
    } else {
        Some(format!(
            "{plugin_id} OAuth sign-in isn't ready in Cockpit yet: missing {}",
            missing.join(", ")
        ))
    }
}

async fn plugin_oauth_client_secret(
    store: &Store,
    auth: &AuthSpec,
) -> anyhow::Result<Option<String>> {
    let Some(key) = auth.client_secret_setting.as_deref() else {
        return Ok(None);
    };
    Ok(store
        .get_setting_raw(key)
        .await?
        .filter(|value| !value.is_empty()))
}

async fn build_plugin_oauth_begin_result(
    store: &Store,
    plugin_id: &str,
    auth: &AuthSpec,
    verifier: &str,
    state_token: &str,
) -> anyhow::Result<PluginOauthBeginResult> {
    let resolved = resolve_plugin_oauth(store, plugin_id, auth).await?;
    build_plugin_oauth_begin_result_with(plugin_id, auth, &resolved, verifier, state_token)
}

/// Build the authorize URL from already-resolved endpoints/client id —
/// table values take precedence over manifest fields (see
/// [`resolve_plugin_oauth`]). `begin_plugin_install` calls this directly
/// with its post-DCR resolution; `begin_plugin_oauth` goes through the
/// async wrapper above.
fn build_plugin_oauth_begin_result_with(
    plugin_id: &str,
    auth: &AuthSpec,
    resolved: &ResolvedPluginOauth,
    verifier: &str,
    state_token: &str,
) -> anyhow::Result<PluginOauthBeginResult> {
    if let Some(message) = plugin_oauth_prereq_error(plugin_id, auth, resolved) {
        anyhow::bail!(message);
    }
    let client_id = resolved
        .client_id
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("{plugin_id} OAuth sign-in is missing a client id"))?;
    let authorize_url = resolved.authorize_url.as_deref().ok_or_else(|| {
        anyhow::anyhow!("{plugin_id} OAuth sign-in is missing auth.authorize_url")
    })?;
    let redirect_uri = plugin_oauth_redirect_uri(plugin_id);
    let requested_scopes = plugin_oauth_requested_scopes(auth);
    let mut url = Url::parse(authorize_url)?;
    {
        let mut query = url.query_pairs_mut();
        query.append_pair("response_type", "code");
        query.append_pair("client_id", client_id);
        query.append_pair("redirect_uri", &redirect_uri);
        query.append_pair("state", state_token);
        query.append_pair("code_challenge", &pkce_challenge_s256(verifier));
        query.append_pair("code_challenge_method", "S256");
        if !requested_scopes.is_empty() {
            query.append_pair("scope", &requested_scopes.join(" "));
        }
        if let Some(resource) = auth.resource.as_deref().filter(|value| !value.is_empty()) {
            query.append_pair("resource", resource);
        }
        for (key, value) in &auth.extra_authorize_params {
            query.append_pair(key, value);
        }
    }

    Ok(PluginOauthBeginResult {
        state_token: state_token.to_string(),
        authorize_url: url.into(),
        redirect_uri,
    })
}

/// Steps 1-6 of the install resolution order: env var → non-oauth kinds →
/// external OAuth → endpoint discovery (regardless of the
/// dynamic-registration flag) → client id / DCR → authorize URL + flow
/// state. Kept free of the Tauri runtime so tests can drive it against a
/// mock vendor; `begin_plugin_install` wraps it with the callback server
/// (step 7) and browser open (step 8).
async fn resolve_plugin_install(
    store: &Store,
    http: &reqwest::Client,
    plugin_id: &str,
    auth: Option<&AuthSpec>,
) -> anyhow::Result<PluginInstallBeginResult> {
    // A manifest without [auth] behaves as kind "none".
    let Some(auth) = auth else {
        return Ok(PluginInstallBeginResult::new("none"));
    };
    let mut result = PluginInstallBeginResult::new(auth_kind_label(auth.kind));
    result.env_var_name = auth.env.clone();

    // 1. Env var short-circuit: install completes with zero auth input (the
    // wizard still routes through the settings step before enabling).
    if auth
        .env
        .as_deref()
        .is_some_and(|e| std::env::var_os(e).is_some())
    {
        result.env_var_present = true;
        return Ok(result);
    }

    // 2. Non-OAuth kinds: the wizard routes to token input or settings.
    if auth.kind != AuthKind::Oauth {
        return Ok(result);
    }

    // 3. External OAuth (google-workspace): no discovery, no browser, no
    // callback — the child server brokers sign-in at first use. The wizard
    // only collects the client id when none is saved yet.
    if is_external_oauth(auth) {
        let resolved = resolve_plugin_oauth(store, plugin_id, auth).await?;
        result.oauth_external = true;
        result.needs_client_id = resolved.client_id.is_none();
        return Ok(result);
    }

    // 4. Endpoint resolution: discover when either endpoint COLUMN is
    // missing — regardless of the dynamic-registration flag (Slack needs
    // endpoints too). Manifest endpoints can still rescue a failure.
    let row = store.get_plugin_oauth_client(plugin_id).await?;
    let row_has_endpoints = row.as_ref().is_some_and(|row| {
        row.authorize_url.as_deref().is_some_and(|v| !v.is_empty())
            && row.token_url.as_deref().is_some_and(|v| !v.is_empty())
    });
    let mut discovered: Option<OauthServerMetadata> = None;
    if !row_has_endpoints {
        if let Some(resource) = auth.resource.as_deref().filter(|v| !v.is_empty()) {
            match discover_oauth_server_metadata(http, resource).await {
                Ok(metadata) => {
                    // Persist endpoints even when registration is impossible
                    // — the manual client-id path needs an authorize URL.
                    // Network I/O above, store write here: never inside
                    // with_conn.
                    store
                        .upsert_plugin_oauth_client(&PluginOauthClient {
                            plugin_id: plugin_id.to_string(),
                            authorize_url: Some(metadata.authorization_endpoint.clone()),
                            token_url: Some(metadata.token_endpoint.clone()),
                            client_id: None,
                        })
                        .await?;
                    discovered = Some(metadata);
                }
                Err(err) => result.dcr_error = Some(err.to_string()),
            }
        }
    }
    let mut resolved = resolve_plugin_oauth(store, plugin_id, auth).await?;
    if resolved.authorize_url.is_none() || resolved.token_url.is_none() {
        // Discovery failed and neither cache nor manifest supplies
        // endpoints — nothing else is possible; the wizard shows dcr_error
        // with Retry.
        return Ok(result);
    }

    // 5. Client id: any existing id (row → client_id_setting) permanently
    // suppresses DCR. DCR runs only when the manifest opts in AND this
    // call's discovery document exposed a registration_endpoint.
    if resolved.client_id.is_none() {
        let registration_endpoint = discovered
            .as_ref()
            .and_then(|m| m.registration_endpoint.clone())
            .filter(|_| auth.dynamic_registration);
        let Some(registration_endpoint) = registration_endpoint else {
            result.needs_client_id = true;
            return Ok(result);
        };
        match register_oauth_client(
            http,
            &registration_endpoint,
            &plugin_oauth_redirect_uri(plugin_id),
        )
        .await
        {
            Ok(client_id) => {
                store
                    .upsert_plugin_oauth_client(&PluginOauthClient {
                        plugin_id: plugin_id.to_string(),
                        authorize_url: None,
                        token_url: None,
                        client_id: Some(client_id.clone()),
                    })
                    .await?;
                result.dcr_succeeded = true;
                resolved.client_id = Some(client_id);
            }
            Err(err) => {
                result.dcr_error = Some(err.to_string());
                result.needs_client_id = true;
                return Ok(result);
            }
        }
    }

    // 6. Authorize URL + flow state; a new begin cancels whatever flow was
    // pending for this plugin first.
    cancel_pending_plugin_flows(plugin_id, None);
    let verifier = generate_pkce_verifier();
    let state_token = ryuzi_core::paths::new_id();
    let begin =
        build_plugin_oauth_begin_result_with(plugin_id, auth, &resolved, &verifier, &state_token)?;
    plugin_oauth_flows()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(
            plugin_oauth_flow_key(plugin_id, &state_token),
            PluginOauthFlowState {
                verifier,
                redirect_uri: begin.redirect_uri.clone(),
                requested_scopes: plugin_oauth_requested_scopes(auth),
            },
        );
    result.oauth_available = true;
    result.oauth_begin = Some(begin);
    // Step 6 succeeded: any earlier dcr_error (e.g. discovery failed but the
    // manifest's endpoints rescued the flow) is stale — never let the DTO
    // carry oauthAvailable:true alongside a leftover dcrError.
    result.dcr_error = None;
    Ok(result)
}

/// Whether an auth block's credential is configured: a persisted, non-empty
/// value under `auth.setting`, or — fallback — the `auth.env` var set in the
/// process environment. Pure so it's testable without a `Store` or a real
/// process environment; callers resolve `setting_value`/`env_is_set` first
/// (see `build_auth_info`).
fn auth_configured(setting_value: Option<&str>, env_is_set: bool) -> bool {
    setting_value.is_some_and(|v| !v.is_empty()) || env_is_set
}

/// `PluginAuthInfo.configured` for the list payload without building the
/// whole auth DTO: oauth → a token is stored and reconnect isn't required;
/// otherwise the `auth.setting`-row / `auth.env` check. No `[auth]` → false.
async fn plugin_auth_configured(
    store: &Store,
    plugin_id: &str,
    auth: Option<&AuthSpec>,
) -> anyhow::Result<bool> {
    let Some(auth) = auth else {
        return Ok(false);
    };
    if auth.kind == AuthKind::Oauth {
        let token = store.get_plugin_oauth_token(plugin_id).await?;
        return Ok(token.is_some_and(|token| !token.reconnect_required));
    }
    let setting_value = match &auth.setting {
        Some(key) => store.get_setting_raw(key).await?,
        None => None,
    };
    let env_is_set = auth
        .env
        .as_deref()
        .is_some_and(|e| std::env::var_os(e).is_some());
    Ok(auth_configured(setting_value.as_deref(), env_is_set))
}

async fn build_auth_info(
    store: &Store,
    plugin_id: &str,
    auth: &AuthSpec,
) -> anyhow::Result<PluginAuthInfo> {
    let setting_value = match &auth.setting {
        Some(key) => store.get_setting_raw(key).await?,
        None => None,
    };
    let env_is_set = auth
        .env
        .as_deref()
        .is_some_and(|e| std::env::var_os(e).is_some());
    let resolved_oauth = if auth.kind == AuthKind::Oauth {
        Some(resolve_plugin_oauth(store, plugin_id, auth).await?)
    } else {
        None
    };
    let oauth_token = if auth.kind == AuthKind::Oauth {
        store.get_plugin_oauth_token(plugin_id).await?
    } else {
        None
    };
    let oauth_reconnect_required = oauth_token
        .as_ref()
        .is_some_and(|token| token.reconnect_required);
    let oauth_token_stored = oauth_token.is_some();
    let oauth_connect_error = resolved_oauth
        .as_ref()
        .and_then(|resolved| plugin_oauth_prereq_error(plugin_id, auth, resolved));
    Ok(PluginAuthInfo {
        kind: auth_kind_label(auth.kind).to_string(),
        setting: auth.setting.clone(),
        env: auth.env.clone(),
        help_url: auth.help_url.clone(),
        configured: if auth.kind == AuthKind::Oauth {
            oauth_token_stored && !oauth_reconnect_required
        } else {
            auth_configured(setting_value.as_deref(), env_is_set)
        },
        oauth_connect_available: auth.kind == AuthKind::Oauth && oauth_connect_error.is_none(),
        oauth_connect_error,
        oauth_token_stored,
        oauth_reconnect_required,
    })
}

/// Whether a settings field's value is set: a persisted, non-empty row.
/// Pure — callers resolve the persisted row first (see `build_settings_info`).
fn field_value_set(persisted: Option<&str>) -> bool {
    persisted.is_some_and(|v| !v.is_empty())
}

async fn build_settings_info(
    store: &Store,
    fields: &[SettingField],
) -> anyhow::Result<Vec<PluginFieldInfo>> {
    let mut out = Vec::with_capacity(fields.len());
    for f in fields {
        let persisted = store.get_setting_raw(&f.key).await?;
        out.push(PluginFieldInfo {
            key: f.key.clone(),
            label: f.label.clone(),
            help: f.help.clone(),
            secret: f.secret,
            required: f.required,
            value_set: field_value_set(persisted.as_deref()),
        });
    }
    Ok(out)
}

fn mcp_transport_label(t: McpTransportDef) -> &'static str {
    match t {
        McpTransportDef::Stdio => "stdio",
        McpTransportDef::Http => "http",
    }
}

/// Raw manifest string, no `${auth}` substitution — command for stdio, url
/// for http.
fn mcp_info(server: &McpServerDef) -> PluginMcpInfo {
    PluginMcpInfo {
        name: server.name.clone(),
        transport: mcp_transport_label(server.transport).to_string(),
        command_or_url: server
            .command
            .clone()
            .or_else(|| server.url.clone())
            .unwrap_or_default(),
    }
}

fn plugin_oauth_auth(cp: &ControlPlane, plugin_id: &str) -> anyhow::Result<AuthSpec> {
    let plugin = cp
        .plugins()
        .get(plugin_id)
        .ok_or_else(|| anyhow::anyhow!("unknown plugin: {plugin_id}"))?;
    let auth = plugin
        .manifest
        .auth
        .as_ref()
        .ok_or_else(|| anyhow::anyhow!("{plugin_id} does not declare an auth block"))?;
    if auth.kind != AuthKind::Oauth {
        anyhow::bail!("{plugin_id} does not use OAuth")
    }
    Ok(auth.clone())
}

async fn exchange_plugin_oauth_code(
    store: &Store,
    plugin_id: &str,
    auth: &AuthSpec,
    flow: &PluginOauthFlowState,
    code: &str,
) -> anyhow::Result<PluginOauthToken> {
    let resolved = resolve_plugin_oauth(store, plugin_id, auth).await?;
    if let Some(message) = plugin_oauth_prereq_error(plugin_id, auth, &resolved) {
        anyhow::bail!(message);
    }
    let client_id = resolved
        .client_id
        .clone()
        .ok_or_else(|| anyhow::anyhow!("{plugin_id} OAuth sign-in is missing a client id"))?;
    let token_url = resolved
        .token_url
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("{plugin_id} OAuth sign-in is missing auth.token_url"))?;
    let client_secret = plugin_oauth_client_secret(store, auth).await?;
    let mut form = vec![
        ("grant_type".to_string(), "authorization_code".to_string()),
        ("code".to_string(), code.to_string()),
        ("redirect_uri".to_string(), flow.redirect_uri.clone()),
        ("client_id".to_string(), client_id),
        ("code_verifier".to_string(), flow.verifier.clone()),
    ];
    if !flow.requested_scopes.is_empty() {
        form.push(("scope".to_string(), flow.requested_scopes.join(" ")));
    }
    if let Some(resource) = auth.resource.as_deref().filter(|value| !value.is_empty()) {
        form.push(("resource".to_string(), resource.to_string()));
    }
    if let Some(secret) = client_secret {
        form.push(("client_secret".to_string(), secret));
    }
    for (key, value) in &auth.extra_token_params {
        form.push((key.clone(), value.clone()));
    }

    let http = reqwest::Client::new();
    let response = http.post(token_url).form(&form).send().await?;
    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        let detail = body.trim();
        if detail.is_empty() {
            anyhow::bail!("{plugin_id} OAuth token exchange failed with HTTP {status}");
        }
        anyhow::bail!("{plugin_id} OAuth token exchange failed with HTTP {status}: {detail}");
    }

    let payload: PluginOauthTokenResponse = response.json().await?;
    let access_token = payload
        .access_token
        .filter(|token| !token.is_empty())
        .ok_or_else(|| {
            anyhow::anyhow!("{plugin_id} OAuth token response is missing access_token")
        })?;
    let token_type = payload
        .token_type
        .filter(|token_type| !token_type.is_empty())
        .unwrap_or_else(|| "Bearer".to_string());
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
        .unwrap_or_else(|| flow.requested_scopes.clone());
    let expires_at = payload
        .expires_in
        .map(|seconds| ryuzi_core::paths::now_ms() + seconds.saturating_mul(1000));

    Ok(PluginOauthToken {
        plugin_id: plugin_id.to_string(),
        access_token,
        refresh_token: payload.refresh_token.filter(|token| !token.is_empty()),
        token_type,
        expires_at,
        scopes,
        reconnect_required: false,
    })
}

/// Consume a captured loopback callback: validate `state` BEFORE touching
/// the flow map (a mismatched code is discarded but the flow entry stays
/// usable for retry/manual paste), exchange the code, store the token.
/// Exchange failure restores the flow entry — the same retry-safe mechanism
/// as `complete_plugin_oauth`.
async fn finish_plugin_oauth_callback(
    store: &Store,
    plugin_id: &str,
    auth: &AuthSpec,
    state_token: &str,
    callback: ryuzi_core::oauth_loopback::CallbackResult,
) -> anyhow::Result<()> {
    let code = callback
        .code
        .ok_or_else(|| anyhow::anyhow!("OAuth callback did not include a `code` parameter"))?;
    let state = callback
        .state
        .ok_or_else(|| anyhow::anyhow!("OAuth callback did not include a `state` parameter"))?;
    if state != state_token {
        anyhow::bail!("OAuth state mismatch — the sign-in response did not match this install");
    }
    let flow_key = plugin_oauth_flow_key(plugin_id, state_token);
    let flow = plugin_oauth_flows()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(&flow_key)
        .ok_or_else(|| anyhow::anyhow!("plugin sign-in flow not found — start again"))?;
    match exchange_plugin_oauth_code(store, plugin_id, auth, &flow, code.trim()).await {
        Ok(token) => {
            store.upsert_plugin_oauth_token(&token).await?;
            Ok(())
        }
        Err(err) => {
            plugin_oauth_flows()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(flow_key, flow);
            Err(err)
        }
    }
}

struct InstalledCtx {
    connections: Vec<ryuzi_core::llm_router::connections::ConnectionRow>,
    installed_skills: Vec<ryuzi_core::skills_install::InstalledSkillInfo>,
}

async fn installed_ctx(store: &Store) -> anyhow::Result<InstalledCtx> {
    Ok(InstalledCtx {
        connections: ryuzi_core::llm_router::connections::list_connections(store).await?,
        installed_skills: ryuzi_core::skills_install::list_installed_skills().unwrap_or_default(),
    })
}

async fn compute_installed(
    store: &Store,
    plugin: &CorePlugin,
    kind: &str,
    enabled: bool,
    configured: bool,
    ctx: &InstalledCtx,
) -> anyhow::Result<bool> {
    let id = &plugin.manifest.id;
    let has_family_connection = kind == "provider" && {
        let family = provider_family(id);
        ctx.connections
            .iter()
            .any(|c| provider_family(&c.provider) == family)
    };
    let gateway_settings_complete = if kind == "gateway" {
        // A gateway with no manifest settings has nothing to configure, so
        // its installed-ness is just whether it's enabled — otherwise it
        // could never leave Browse. Discord (the only gateway today) has 3
        // required settings and takes the all-present path below.
        if plugin.manifest.settings.is_empty() {
            enabled
        } else {
            let mut complete = true;
            for field in &plugin.manifest.settings {
                let value = store.get_setting_raw(&field.key).await?;
                if value.as_deref().map(str::trim).is_none_or(str::is_empty) {
                    complete = false;
                    break;
                }
            }
            complete
        }
    } else {
        false
    };
    let skill_pack_installed = kind == "skill-pack"
        && ctx
            .installed_skills
            .iter()
            .any(|s| s.plugin_id.as_deref() == Some(id.as_str()) || &s.id == id);
    Ok(installed_flag(
        kind,
        enabled,
        configured,
        has_family_connection,
        gateway_settings_complete,
        skill_pack_installed,
    ))
}

/// Fetch every `plugin_installs` ledger row ONCE and index it by plugin id —
/// mirrors `serve.rs`'s `list_plugins` single-query-then-index approach so
/// list assembly stays O(1) round-trips regardless of the plugin count
/// (never a per-plugin `get_plugin_install` inside the loop below).
async fn install_ledger_index(
    store: &Store,
) -> anyhow::Result<HashMap<String, ryuzi_core::store::PluginInstallRecord>> {
    Ok(store
        .list_plugin_installs()
        .await?
        .into_iter()
        .map(|r| (r.plugin_id.clone(), r))
        .collect())
}

async fn assemble_list(cp: &ControlPlane) -> anyhow::Result<Vec<PluginInfo>> {
    let settings = SettingsStore::new(cp.store().clone());
    let ctx = installed_ctx(cp.store()).await?;
    let installs = install_ledger_index(cp.store()).await?;
    let mut out = Vec::new();
    for plugin in cp.plugins().list() {
        let Some(kind) = derive_kind(&plugin) else {
            continue;
        };
        let enabled = cp
            .plugins()
            .is_enabled(&settings, &plugin.manifest.id)
            .await?;
        let configured = plugin_auth_configured(
            cp.store(),
            &plugin.manifest.id,
            plugin.manifest.auth.as_ref(),
        )
        .await?;
        let installed =
            compute_installed(cp.store(), &plugin, kind, enabled, configured, &ctx).await?;
        let record = installs.get(&plugin.manifest.id);
        out.push(plugin_info(
            &plugin, enabled, configured, kind, installed, record,
        ));
    }
    for pack in ryuzi_core::skills_install::curated_skill_packs() {
        if cp.plugins().get(pack.id).is_some() || out.iter().any(|p| p.id == pack.id) {
            continue;
        }
        let installed = ctx
            .installed_skills
            .iter()
            .any(|s| s.id == pack.id || s.source == pack.id || s.source == pack.repo);
        let ledger = InstallLedgerFields::from_option(installs.get(pack.id));
        out.push(PluginInfo {
            id: pack.id.to_string(),
            name: pack.name.to_string(),
            description: pack.description.to_string(),
            icon: Some("sparkles".to_string()),
            categories: vec!["skills".to_string()],
            verified: true,
            experimental: false,
            // A synthesized pack isn't a registered plugin, so `enabled` /
            // `configured` are meaningless here — only `installed` drives the
            // Browse/Installed split.
            enabled: false,
            configured: false,
            source: "skill-pack".to_string(),
            capabilities: vec![],
            kind: "skill-pack".to_string(),
            installed,
            family: None,
            pinned: ledger.pinned,
            source_spec: ledger.source_spec,
            resolved_commit: ledger.resolved_commit,
            installed_at: ledger.installed_at,
            updated_at: ledger.updated_at,
            trust_tier: ledger.trust_tier,
        });
    }
    Ok(out)
}

async fn assemble_detail(cp: &ControlPlane, id: &str) -> anyhow::Result<PluginDetail> {
    let Some(plugin) = cp.plugins().get(id) else {
        anyhow::bail!("unknown plugin: {id}");
    };
    let settings = SettingsStore::new(cp.store().clone());
    let enabled = cp.plugins().is_enabled(&settings, id).await?;
    let m = &plugin.manifest;

    let auth = match &m.auth {
        Some(auth) => Some(build_auth_info(cp.store(), id, auth).await?),
        None => None,
    };
    let settings_info = build_settings_info(cp.store(), &m.settings).await?;
    let mcp = m.mcp.iter().map(mcp_info).collect();
    let models = providers::list_models(cp.store(), id).await?;
    let configured = plugin_auth_configured(cp.store(), id, m.auth.as_ref()).await?;
    let kind = derive_kind(&plugin).unwrap_or("integration");
    let ctx = installed_ctx(cp.store()).await?;
    let installed = compute_installed(cp.store(), &plugin, kind, enabled, configured, &ctx).await?;
    // Single-plugin lookup is fine here — unlike `assemble_list`, there is
    // only ever one id to resolve for a detail view.
    let record = cp.store().get_plugin_install(id).await?;

    Ok(PluginDetail {
        info: plugin_info(
            &plugin,
            enabled,
            configured,
            kind,
            installed,
            record.as_ref(),
        ),
        auth,
        settings: settings_info,
        mcp,
        models,
        homepage: m.homepage.clone(),
        publisher: m.publisher.clone(),
    })
}

#[tauri::command]
#[specta::specta]
pub async fn list_plugins(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<PluginInfo>> {
    Ok(assemble_list(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn plugin_detail(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<PluginDetail> {
    Ok(assemble_detail(&cp, &id).await?)
}

/// Same semantics as `ryuzi plugins enable/disable` — delegates to the
/// shared core helper so the two surfaces never drift.
#[tauri::command]
#[specta::specta]
pub async fn set_plugin_enabled(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    enabled: bool,
) -> R<()> {
    let settings = SettingsStore::new(cp.store().clone());
    ryuzi_core::plugins::toggle_enabled(cp.plugins(), &settings, &id, enabled).await?;
    Ok(())
}

/// Validated write through `SettingsStore::set` — rejects unknown keys and
/// type-mismatched values the same way `ryuzi config set` does. Never
/// returns a value, so no secret can leak back through this command.
#[tauri::command]
#[specta::specta]
pub async fn set_plugin_setting(
    cp: State<'_, Arc<ControlPlane>>,
    key: String,
    value: String,
) -> R<()> {
    let settings = SettingsStore::new(cp.store().clone());
    settings.set(&key, &value).await?;
    Ok(())
}

/// Kind-symmetric uninstall: after this the entry's `installed` flips false
/// and it reappears in Browse.
async fn uninstall(cp: &ControlPlane, id: &str) -> anyhow::Result<()> {
    let settings = SettingsStore::new(cp.store().clone());
    let Some(plugin) = cp.plugins().get(id) else {
        // Synthesized curated pack or a pack installed without a manifest —
        // resolve through the skills installer.
        let installed = ryuzi_core::skills_install::list_installed_skills()?;
        let Some(pack) = installed
            .iter()
            .find(|s| s.id == id || s.source == id || s.plugin_id.as_deref() == Some(id))
        else {
            anyhow::bail!("unknown plugin: {id}");
        };
        return ryuzi_core::skills_install::remove_installed_skill(&pack.id);
    };
    match derive_kind(&plugin) {
        Some("provider") => {
            let family = provider_family(id);
            for row in ryuzi_core::llm_router::connections::list_connections(cp.store()).await? {
                if provider_family(&row.provider) == family {
                    ryuzi_core::llm_router::connections::remove_connection(cp.store(), &row.id)
                        .await?;
                }
            }
            Ok(())
        }
        Some("gateway") => {
            for field in &plugin.manifest.settings {
                cp.store().delete_setting_raw(&field.key).await?;
            }
            ryuzi_core::plugins::toggle_enabled(cp.plugins(), &settings, id, false).await
        }
        Some("skill-pack") => {
            let installed = ryuzi_core::skills_install::list_installed_skills()?;
            let Some(pack) = installed
                .iter()
                .find(|s| s.plugin_id.as_deref() == Some(id) || s.id == id)
            else {
                anyhow::bail!("skill pack not installed: {id}");
            };
            ryuzi_core::skills_install::remove_installed_skill(&pack.id)
        }
        _ => {
            if let Some(auth) = &plugin.manifest.auth {
                if let Some(setting) = &auth.setting {
                    cp.store().delete_setting_raw(setting).await?;
                }
                if auth.kind == AuthKind::Oauth {
                    cp.store().delete_plugin_oauth_token(id).await?;
                }
            }
            for field in &plugin.manifest.settings {
                cp.store().delete_setting_raw(&field.key).await?;
            }
            if plugin.connector.is_some() && !plugin.manifest.experimental {
                ryuzi_core::plugins::toggle_enabled(cp.plugins(), &settings, id, false).await?;
            }
            Ok(())
        }
    }
}

/// `uninstall` plus marking `plugins_restart_required` — split out (taking
/// `&ControlPlane` rather than the command's `State`) so it's directly
/// testable with `test_cp()`, the same way `uninstall` itself already is.
async fn uninstall_and_mark(cp: &ControlPlane, id: &str) -> anyhow::Result<()> {
    uninstall(cp, id).await?;
    cp.mark_plugins_restart_required();
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn uninstall_plugin(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<Vec<PluginInfo>> {
    uninstall_and_mark(&cp, &id).await?;
    Ok(assemble_list(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn begin_plugin_oauth(
    app: AppHandle,
    cp: State<'_, Arc<ControlPlane>>,
    plugin_id: String,
) -> R<PluginOauthBeginResult> {
    let auth = plugin_oauth_auth(&cp, &plugin_id)?;
    let verifier = generate_pkce_verifier();
    let state_token = ryuzi_core::paths::new_id();
    let begin =
        build_plugin_oauth_begin_result(cp.store(), &plugin_id, &auth, &verifier, &state_token)
            .await?;
    plugin_oauth_flows()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(
            plugin_oauth_flow_key(&plugin_id, &state_token),
            PluginOauthFlowState {
                verifier,
                redirect_uri: begin.redirect_uri.clone(),
                requested_scopes: plugin_oauth_requested_scopes(&auth),
            },
        );
    let _ = PluginOauthAuthorizeUrlMsg {
        plugin_id: plugin_id.clone(),
        authorize_url: begin.authorize_url.clone(),
    }
    .emit(&app);
    let _ = app
        .opener()
        .open_url(begin.authorize_url.clone(), None::<&str>);
    Ok(begin)
}

#[tauri::command]
#[specta::specta]
pub async fn complete_plugin_oauth(
    cp: State<'_, Arc<ControlPlane>>,
    plugin_id: String,
    code: String,
    state_token: String,
) -> R<PluginAuthInfo> {
    let auth = plugin_oauth_auth(&cp, &plugin_id)?;
    let flow_key = plugin_oauth_flow_key(&plugin_id, &state_token);
    let flow = plugin_oauth_flows()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(&flow_key)
        .ok_or_else(|| CmdError {
            message: "plugin sign-in flow not found — start again".into(),
        })?;
    let token =
        match exchange_plugin_oauth_code(cp.store(), &plugin_id, &auth, &flow, code.trim()).await {
            Ok(token) => token,
            Err(err) => {
                plugin_oauth_flows()
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .insert(flow_key, flow);
                return Err(err.into());
            }
        };
    cp.store().upsert_plugin_oauth_token(&token).await?;
    Ok(build_auth_info(cp.store(), &plugin_id, &auth).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn disconnect_plugin_oauth(
    cp: State<'_, Arc<ControlPlane>>,
    plugin_id: String,
) -> R<PluginAuthInfo> {
    let auth = plugin_oauth_auth(&cp, &plugin_id)?;
    cp.store().delete_plugin_oauth_token(&plugin_id).await?;
    Ok(build_auth_info(cp.store(), &plugin_id, &auth).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn plugin_models(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<Vec<String>> {
    Ok(providers::list_models(cp.store(), &id).await?)
}

/// The install wizard's entry point (spec 8-step resolution order). Steps
/// 1-6 live in `resolve_plugin_install`; this command adds step 7 (bind
/// 8976 — retried briefly — + background callback/exchange task, degrading
/// to `callback_mode: "manual"` only when the port is still taken after
/// the retries) and step 8 (emit `PluginOauthAuthorizeUrlMsg` + open the
/// browser), exactly like `begin_plugin_oauth` does for the detail view.
#[tauri::command]
#[specta::specta]
pub async fn begin_plugin_install(
    app: AppHandle,
    cp: State<'_, Arc<ControlPlane>>,
    plugin_id: String,
) -> R<PluginInstallBeginResult> {
    let plugin = cp.plugins().get(&plugin_id).ok_or_else(|| CmdError {
        message: format!("unknown plugin: {plugin_id}"),
    })?;
    let auth = plugin.manifest.auth.clone();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| CmdError {
            message: e.to_string(),
        })?;
    let mut result = resolve_plugin_install(cp.store(), &http, &plugin_id, auth.as_ref()).await?;
    let Some(begin) = result.oauth_begin.clone() else {
        return Ok(result);
    };
    let auth = auth.expect("a prepared oauth flow implies an auth block");

    // 7. Register a cancel handle for this flow BEFORE the bind-retry loop
    // below: cancel_plugin_install can arrive while that loop is still
    // running (worst case ~300ms across the 3 attempts), and if the sender
    // isn't in the map yet, cancel_pending_plugin_flows finds nothing to
    // signal — the flow entry is removed but the eventually-spawned
    // background task never learns it was canceled and holds the port for
    // the full 5-minute timeout. Registering first means a cancel that
    // fires during the bind window pre-fires cancel_rx, and the spawned
    // task's `tokio::select!` on an already-fired oneshot resolves on its
    // first poll — no window is lost.
    let (cancel_tx, cancel_rx) = tokio::sync::oneshot::channel::<()>();
    let flow_key = plugin_oauth_flow_key(&plugin_id, &begin.state_token);
    plugin_install_cancels()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(flow_key.clone(), cancel_tx);

    // Callback server on the fixed wizard port. A same-plugin re-begin
    // (Retry) has just SIGNALED the previous flow's callback server via
    // cancel_pending_plugin_flows (step 6), but that axum server shuts down
    // asynchronously — the port can still be held for a moment. Retry the
    // bind briefly (3 attempts, 100ms apart) before concluding the port is
    // genuinely taken by another flow.
    let mut bound = ryuzi_core::oauth_loopback::bind_fixed(PLUGIN_OAUTH_CALLBACK_PORT).await;
    for _ in 0..2 {
        if bound.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        bound = ryuzi_core::oauth_loopback::bind_fixed(PLUGIN_OAUTH_CALLBACK_PORT).await;
    }
    match bound {
        Ok(listener) => {
            result.callback_mode = "auto".to_string();
            let (server, result_rx, shutdown_tx) =
                ryuzi_core::oauth_loopback::spawn_callback_server(
                    listener,
                    &plugin_oauth_callback_path(&plugin_id),
                );
            let store = cp.store().clone();
            let app_handle = app.clone();
            let task_plugin_id = plugin_id.clone();
            let state_token = begin.state_token.clone();
            tauri::async_runtime::spawn(async move {
                let outcome = tokio::select! {
                    res = ryuzi_core::oauth_loopback::await_callback(
                        server,
                        result_rx,
                        shutdown_tx,
                        std::time::Duration::from_secs(5 * 60),
                    ) => Some(res),
                    // Cancellation (wizard closed / re-begin): dropping the
                    // await_callback future drops shutdown_tx, which shuts
                    // the axum server down gracefully.
                    _ = cancel_rx => None,
                };
                plugin_install_cancels()
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .remove(&flow_key);
                // Canceled: exit silently — the wizard initiated it.
                let Some(res) = outcome else { return };
                let msg = match res {
                    Ok(callback) => {
                        match finish_plugin_oauth_callback(
                            &store,
                            &task_plugin_id,
                            &auth,
                            &state_token,
                            callback,
                        )
                        .await
                        {
                            Ok(()) => PluginOauthCompletedMsg {
                                plugin_id: task_plugin_id.clone(),
                                ok: true,
                                error: None,
                            },
                            Err(err) => PluginOauthCompletedMsg {
                                plugin_id: task_plugin_id.clone(),
                                ok: false,
                                error: Some(err.to_string()),
                            },
                        }
                    }
                    Err(err) => PluginOauthCompletedMsg {
                        plugin_id: task_plugin_id.clone(),
                        ok: false,
                        error: Some(err.to_string()),
                    },
                };
                let _ = msg.emit(&app_handle);
            });
        }
        Err(_) => {
            // Port still taken after the retries — e.g. another plugin's
            // flow is pending. No background task will ever be spawned to
            // consume cancel_rx in this branch, so remove the sender
            // registered above to avoid leaking it in the cancels map. The
            // wizard explains why; completion goes through
            // complete_plugin_oauth.
            plugin_install_cancels()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&flow_key);
            result.callback_mode = "manual".to_string();
        }
    }

    // 8. Emit the authorize URL and open the browser (as begin_plugin_oauth
    // does today).
    let _ = PluginOauthAuthorizeUrlMsg {
        plugin_id: plugin_id.clone(),
        authorize_url: begin.authorize_url.clone(),
    }
    .emit(&app);
    let _ = app
        .opener()
        .open_url(begin.authorize_url.clone(), None::<&str>);
    Ok(result)
}

/// Persist a manually-entered client id. External-OAuth plugins store it
/// under the declared `auth.setting` via the validated SettingsStore path
/// (`validate_setting`/`register_plugin_fields` only accept
/// manifest-declared keys); everyone else upserts
/// `plugin_oauth_clients.client_id` — deliberately NOT a `plugin.*` setting,
/// since none of these manifests declare one.
async fn set_plugin_oauth_client_id_inner(
    cp: &ControlPlane,
    plugin_id: &str,
    client_id: &str,
) -> anyhow::Result<()> {
    let auth = plugin_oauth_auth(cp, plugin_id)?;
    let client_id = client_id.trim();
    if client_id.is_empty() {
        anyhow::bail!("client id must not be empty");
    }
    if is_external_oauth(&auth) {
        let Some(key) = auth.setting.as_deref() else {
            anyhow::bail!("{plugin_id} declares no auth.setting to hold a client id");
        };
        let settings = SettingsStore::new(cp.store().clone());
        settings.set(key, client_id).await?;
        return Ok(());
    }
    cp.store()
        .upsert_plugin_oauth_client(&PluginOauthClient {
            plugin_id: plugin_id.to_string(),
            authorize_url: None,
            token_url: None,
            client_id: Some(client_id.to_string()),
        })
        .await?;
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn set_plugin_oauth_client_id(
    cp: State<'_, Arc<ControlPlane>>,
    plugin_id: String,
    client_id: String,
) -> R<()> {
    Ok(set_plugin_oauth_client_id_inner(&cp, &plugin_id, &client_id).await?)
}

/// Cancel the pending OAuth flow for this plugin, if any: shuts down the
/// callback listener and removes the flow-map entry. `state_token` narrows
/// to a specific flow when known; `None` cancels whatever is pending for
/// the id.
#[tauri::command]
#[specta::specta]
pub async fn cancel_plugin_install(
    cp: State<'_, Arc<ControlPlane>>,
    plugin_id: String,
    state_token: Option<String>,
) -> R<()> {
    if cp.plugins().get(&plugin_id).is_none() {
        return Err(CmdError {
            message: format!("unknown plugin: {plugin_id}"),
        });
    }
    cancel_pending_plugin_flows(&plugin_id, state_token.as_deref());
    Ok(())
}

// ---------- Skill/plugin distribution: trust prompt, update, pin, doctor ----------
//
// The DTOs below are thin camelCase mirrors of `ryuzi_core::skills_install`'s
// `TrustPrompt`/`UpdateOutcome` and `ryuzi_core::plugins::doctor`'s
// `DoctorFinding` — those core types derive `Serialize`/`Deserialize` but not
// specta's `Type`, so they cannot cross the Tauri IPC boundary directly (same
// rationale as `PluginInfo` mirroring `ryuzi_plugin_sdk::PluginManifest`
// above). None of these add or drop any field relative to the core type, and
// `TrustPrompt` is already secret-free by construction (repo path, skill
// names, hook-script paths, byte count — no credential material).

/// Mirror of `ryuzi_core::skills_install::TrustPrompt`. `total_bytes` stays a
/// `u64` (not narrowed to `u32`) to avoid silently truncating a large pack's
/// byte count — `export_bindings`'s `BigIntExportBehavior::Number` already
/// renders any bigint-sized field as a plain TS `number`, so there's no
/// bindings-shape cost to keeping the wider type.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct TrustPromptDto {
    pub token: String,
    pub source_spec: String,
    pub owner_repo: String,
    pub resolved_commit: Option<String>,
    pub skills: Vec<String>,
    pub hook_scripts: Vec<String>,
    pub total_bytes: u64,
}

impl From<TrustPrompt> for TrustPromptDto {
    fn from(p: TrustPrompt) -> Self {
        TrustPromptDto {
            token: p.token,
            source_spec: p.source_spec,
            owner_repo: p.owner_repo,
            resolved_commit: p.resolved_commit,
            skills: p.skills,
            hook_scripts: p.hook_scripts,
            total_bytes: p.total_bytes,
        }
    }
}

/// Mirror of `ryuzi_core::skills_install::BeginInstall`, flattened into a
/// single `{completed, trust?, plugin?}` shape the wizard can branch on
/// without a tagged-union match in TS. `trust` is set for
/// `NeedsConfirmation`, `plugin` for `Completed` — exactly one is ever
/// `Some`.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct SkillInstallBegin {
    pub completed: bool,
    pub trust: Option<TrustPromptDto>,
    pub plugin: Option<InstalledSkillPack>,
}

impl SkillInstallBegin {
    fn from_completed(pack: InstalledSkillPack) -> Self {
        SkillInstallBegin {
            completed: true,
            trust: None,
            plugin: Some(pack),
        }
    }

    fn from_needs_confirmation(prompt: TrustPrompt) -> Self {
        SkillInstallBegin {
            completed: false,
            trust: Some(TrustPromptDto::from(prompt)),
            plugin: None,
        }
    }
}

/// Mirror of `ryuzi_core::skills_install::UpdateOutcome`. Keeps the same
/// `#[serde(tag = "kind", content = "detail")]` shape so the discriminated
/// union round-trips identically to the core enum.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase", tag = "kind", content = "detail")]
pub enum UpdateOutcomeDto {
    Updated,
    AlreadyCurrent,
    SkippedPinned,
    LocalEdits,
    Failed(String),
    NeedsReack(TrustPromptDto),
}

impl From<UpdateOutcome> for UpdateOutcomeDto {
    fn from(outcome: UpdateOutcome) -> Self {
        match outcome {
            UpdateOutcome::Updated => UpdateOutcomeDto::Updated,
            UpdateOutcome::AlreadyCurrent => UpdateOutcomeDto::AlreadyCurrent,
            UpdateOutcome::SkippedPinned => UpdateOutcomeDto::SkippedPinned,
            UpdateOutcome::LocalEdits => UpdateOutcomeDto::LocalEdits,
            UpdateOutcome::Failed(message) => UpdateOutcomeDto::Failed(message),
            UpdateOutcome::NeedsReack(prompt) => {
                UpdateOutcomeDto::NeedsReack(TrustPromptDto::from(prompt))
            }
        }
    }
}

/// One pack's outcome from `update_all_plugins` — `ryuzi_core::skills_install
/// ::update_all_packs` returns `Vec<(String, UpdateOutcome)>`; specta can't
/// name a bare tuple usefully in the generated TS, so this wraps it in a
/// named struct.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct UpdateOutcomeEntry {
    pub id: String,
    pub outcome: UpdateOutcomeDto,
}

/// Mirror of `ryuzi_core::plugins::doctor::DoctorFinding`. Already
/// secret-free at the source (see that module's doc comment) — this DTO adds
/// no new fields, just the specta `Type` the core struct doesn't derive.
#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DoctorFinding {
    pub plugin_id: String,
    /// `warn` | `error`.
    pub severity: String,
    /// `reconnect-required` | `missing-binary` | `attach-failed`.
    pub kind: String,
    pub message: String,
    pub suggested_action: String,
}

impl From<doctor::DoctorFinding> for DoctorFinding {
    fn from(f: doctor::DoctorFinding) -> Self {
        DoctorFinding {
            plugin_id: f.plugin_id,
            severity: f.severity,
            kind: f.kind,
            message: f.message,
            suggested_action: f.suggested_action,
        }
    }
}

/// Pure mapping layer over `skills_install::begin_install`, split out so it's
/// unit-testable against a hermetic `Store` without a Tauri `State`. Marks
/// the daemon dirty (`plugins_restart_required`) only when the install
/// actually completed — see `mark_restart_if_begin_completed`.
async fn begin_skill_install_inner(
    source: &str,
    cp: &ControlPlane,
) -> anyhow::Result<SkillInstallBegin> {
    let result = ryuzi_core::skills_install::begin_install(source, cp.store()).await?;
    mark_restart_if_begin_completed(cp, &result);
    Ok(match result {
        BeginInstall::Completed(pack) => SkillInstallBegin::from_completed(pack),
        BeginInstall::NeedsConfirmation(prompt) => {
            SkillInstallBegin::from_needs_confirmation(prompt)
        }
    })
}

/// Whether a `begin_install` result should flip the in-memory
/// `plugins_restart_required` latch — only true for a `Completed` install (a
/// `NeedsConfirmation` trust prompt hasn't touched disk yet). Split out from
/// `begin_skill_install_inner` so it's testable without a hermetic,
/// network-free way to drive `begin_install`'s `Completed` branch (curated
/// installs still resolve through `InstallRoots::for_user()` and a real git
/// clone — see that function's test for why it isn't driven directly here).
fn mark_restart_if_begin_completed(cp: &ControlPlane, result: &BeginInstall) {
    if matches!(result, BeginInstall::Completed(_)) {
        cp.mark_plugins_restart_required();
    }
}

/// Phase 1 of the two-phase tiered trust gate (see
/// `ryuzi_core::skills_install::begin_install`): curated sources install
/// immediately (`completed: true`); arbitrary sources stop at a trust prompt
/// the wizard must show before `confirm_skill_install` can proceed.
#[tauri::command]
#[specta::specta]
pub async fn begin_skill_install(
    cp: State<'_, Arc<ControlPlane>>,
    source: String,
) -> R<SkillInstallBegin> {
    Ok(begin_skill_install_inner(&source, &cp).await?)
}

/// Phase 2: complete a staged install (or update) after the user has
/// acknowledged its `TrustPromptDto`. The token is single-use — see
/// `ryuzi_core::skills_install::confirm_install`. Always marks
/// `plugins_restart_required`: reaching this point always means an install
/// (or reack-triggered update) just completed.
#[tauri::command]
#[specta::specta]
pub async fn confirm_skill_install(
    cp: State<'_, Arc<ControlPlane>>,
    token: String,
) -> R<InstalledSkillPack> {
    let pack = ryuzi_core::skills_install::confirm_install(&token, cp.store()).await?;
    cp.mark_plugins_restart_required();
    Ok(pack)
}

/// Whether an `UpdateOutcome` should flip `plugins_restart_required` — only
/// `Updated` actually reinstalls anything on disk;
/// `AlreadyCurrent`/`SkippedPinned`/`LocalEdits`/`Failed`/`NeedsReack` are all
/// no-ops. Mirrors `serve.rs`'s `update_plugin_route`.
fn is_restart_required_update(outcome: &UpdateOutcome) -> bool {
    matches!(outcome, UpdateOutcome::Updated)
}

/// Update one installed pack. `force` overrides the local-edits guard but
/// never the pinned guard or the hook-script re-ack gate — see
/// `ryuzi_core::skills_install::update_installed_pack`'s decision order.
#[tauri::command]
#[specta::specta]
pub async fn update_plugin(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    force: bool,
) -> R<UpdateOutcomeDto> {
    let outcome = ryuzi_core::skills_install::update_installed_pack(&id, force, cp.store()).await?;
    if is_restart_required_update(&outcome) {
        cp.mark_plugins_restart_required();
    }
    Ok(UpdateOutcomeDto::from(outcome))
}

/// Whether at least one pack in an `update_all_packs` batch actually
/// reinstalled — the batch as a whole only needs a restart if one did.
fn any_update_requires_restart(results: &[(String, UpdateOutcome)]) -> bool {
    results.iter().any(|(_, o)| is_restart_required_update(o))
}

/// Update every installed pack (skipping pinned ones); never fails as a
/// whole — a single pack's error surfaces as that pack's
/// `UpdateOutcomeDto::Failed` entry.
#[tauri::command]
#[specta::specta]
pub async fn update_all_plugins(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<UpdateOutcomeEntry>> {
    let results = ryuzi_core::skills_install::update_all_packs(cp.store()).await?;
    if any_update_requires_restart(&results) {
        cp.mark_plugins_restart_required();
    }
    Ok(results
        .into_iter()
        .map(|(id, outcome)| UpdateOutcomeEntry {
            id,
            outcome: UpdateOutcomeDto::from(outcome),
        })
        .collect())
}

/// Pin (or unpin) an installed pack against future updates.
#[tauri::command]
#[specta::specta]
pub async fn set_plugin_pin(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    pinned: bool,
    reason: Option<String>,
) -> R<()> {
    Ok(
        ryuzi_core::skills_install::set_pack_pin(&id, pinned, reason.as_deref(), cp.store())
            .await?,
    )
}

/// Read-only plugin health aggregation — see
/// `ryuzi_core::plugins::doctor::plugin_doctor`'s doc comment for the full
/// list of checks. Never mutates state.
#[tauri::command]
#[specta::specta]
pub async fn plugin_doctor(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<DoctorFinding>> {
    let findings = doctor::plugin_doctor(&cp).await?;
    Ok(findings.into_iter().map(DoctorFinding::from).collect())
}

/// Whether a plugin install/update since the last app start requires a
/// restart to take effect (in-memory flag on `ControlPlane`, cleared only by
/// process restart).
#[tauri::command]
#[specta::specta]
pub async fn plugins_restart_required(cp: State<'_, Arc<ControlPlane>>) -> R<bool> {
    Ok(cp.plugins_restart_required())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ryuzi_core::connector::{Connector, ConnectorCtx};
    use ryuzi_core::domain::McpServerSpec;
    use ryuzi_core::gateway::{Gateway, GatewayFactory};
    use ryuzi_core::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx};
    use ryuzi_core::Registries;
    use ryuzi_plugin_sdk::{AuthSpec, ModelDef, PluginManifest, ProviderMeta, RuntimeMeta};
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ---- minimal fakes, self-contained to this test module ----

    struct FakeHarness;
    #[async_trait::async_trait]
    impl Harness for FakeHarness {
        async fn start_session(&self, _ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            anyhow::bail!("not needed in this test")
        }
    }
    struct FakeHarnessFactory;
    impl HarnessFactory for FakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(FakeHarness))
        }
    }

    struct FakeGateway;
    #[async_trait::async_trait]
    impl Gateway for FakeGateway {
        fn id(&self) -> &str {
            "fake"
        }
        async fn start(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn create_workspace(&self, name: &str) -> anyhow::Result<String> {
            Ok(format!("ws-{name}"))
        }
        async fn create_conversation(
            &self,
            _workspace_id: &str,
            _title: &str,
        ) -> anyhow::Result<String> {
            Ok("conv".to_string())
        }
        async fn post_status(
            &self,
            surface: &ryuzi_core::domain::Surface,
            _text: &str,
        ) -> anyhow::Result<ryuzi_core::gateway::MessageRef> {
            Ok(ryuzi_core::gateway::MessageRef {
                surface: surface.clone(),
                message_id: "m1".to_string(),
            })
        }
        async fn edit_status(
            &self,
            _msg: &ryuzi_core::gateway::MessageRef,
            _text: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_result(
            &self,
            _surface: &ryuzi_core::domain::Surface,
            _chunks: &[String],
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_error(
            &self,
            _surface: &ryuzi_core::domain::Surface,
            _message: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _s: &ryuzi_core::domain::Surface,
            _r: &ryuzi_core::ApprovalRequest,
        ) -> anyhow::Result<ryuzi_core::ApprovalDecision> {
            Ok(ryuzi_core::ApprovalDecision::Cancel)
        }
    }
    struct FakeGatewayFactory;
    impl GatewayFactory for FakeGatewayFactory {
        fn create(&self, _c: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>> {
            Ok(Arc::new(FakeGateway))
        }
    }

    struct FakeConnector;
    #[async_trait::async_trait]
    impl Connector for FakeConnector {
        async fn mcp_servers(&self, _ctx: &ConnectorCtx) -> anyhow::Result<Vec<McpServerSpec>> {
            Ok(vec![])
        }
    }

    fn manifest(id: &str) -> PluginManifest {
        PluginManifest {
            contract: 1,
            id: id.to_string(),
            name: format!("Plugin {id}"),
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
            provider: None,
            runtime: None,
        }
    }

    fn harness_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: Some(Arc::new(FakeHarnessFactory)),
            gateway: None,
            connector: None,
            source: PluginSource::Builtin,
        }
    }

    fn gateway_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: Some(Arc::new(FakeGatewayFactory)),
            connector: None,
            source: PluginSource::Catalog,
        }
    }

    fn connector_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: None,
            connector: Some(Arc::new(FakeConnector)),
            source: PluginSource::SkillPack(std::path::PathBuf::from("/tmp/whatever")),
        }
    }

    fn manifest_only_with_runtime_meta(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: PluginManifest {
                runtime: Some(RuntimeMeta {
                    binary: Some("acme".to_string()),
                    npm_package: None,
                    default_model: None,
                }),
                ..manifest(id)
            },
            harness: None,
            gateway: None,
            connector: None,
            source: PluginSource::Builtin,
        }
    }

    fn provider_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: PluginManifest {
                provider: Some(ProviderMeta {
                    format: "openai".to_string(),
                    base_url: None,
                    models: vec![ModelDef {
                        id: "m1".to_string(),
                        label: None,
                        default: true,
                    }],
                }),
                ..manifest(id)
            },
            harness: None,
            gateway: None,
            connector: None,
            source: PluginSource::Builtin,
        }
    }

    // ---------- capabilities ----------

    #[test]
    fn capabilities_provider_from_manifest() {
        assert_eq!(provider_only("p").capabilities(), vec!["provider"]);
    }

    #[test]
    fn capabilities_runtime_from_live_harness() {
        assert_eq!(harness_only("h").capabilities(), vec!["runtime"]);
    }

    #[test]
    fn capabilities_runtime_from_manifest_only_runtime_meta() {
        assert_eq!(
            manifest_only_with_runtime_meta("r").capabilities(),
            vec!["runtime"]
        );
    }

    #[test]
    fn capabilities_gateway_from_live_gateway() {
        assert_eq!(gateway_only("g").capabilities(), vec!["gateway"]);
    }

    #[test]
    fn capabilities_connector_from_live_connector() {
        assert_eq!(connector_only("c").capabilities(), vec!["connector"]);
    }

    #[test]
    fn capabilities_empty_for_manifest_only_plugin() {
        assert!(CorePlugin {
            manifest: manifest("m"),
            harness: None,
            gateway: None,
            connector: None,
            source: PluginSource::Builtin,
        }
        .capabilities()
        .is_empty());
    }

    // ---------- source_label ----------

    #[test]
    fn source_label_maps_every_variant() {
        assert_eq!(source_label(&PluginSource::Builtin), "builtin");
        assert_eq!(source_label(&PluginSource::Catalog), "catalog");
        assert_eq!(
            source_label(&PluginSource::SkillPack(std::path::PathBuf::from("/x"))),
            "skill-pack"
        );
    }

    // ---------- derive_kind ----------

    #[test]
    fn derive_kind_classifies_each_capability_shape() {
        assert_eq!(derive_kind(&provider_only("anthropic")), Some("provider"));
        assert_eq!(derive_kind(&gateway_only("discord")), Some("gateway"));
        assert_eq!(derive_kind(&connector_only("slack")), Some("integration"));
        assert_eq!(derive_kind(&harness_only("native")), None);
        assert_eq!(derive_kind(&manifest_only_with_runtime_meta("codex")), None);
    }

    #[test]
    fn derive_kind_skill_pack_from_source() {
        let mut plugin = connector_only("acme-pack");
        plugin.source = PluginSource::SkillPack(std::path::PathBuf::from("/tmp/p"));
        assert_eq!(derive_kind(&plugin), Some("skill-pack"));
    }

    // ---------- installed_flag ----------

    #[test]
    fn installed_flag_per_kind() {
        assert!(installed_flag(
            "integration",
            true,
            false,
            false,
            false,
            false
        ));
        assert!(installed_flag(
            "integration",
            false,
            true,
            false,
            false,
            false
        ));
        assert!(!installed_flag(
            "integration",
            false,
            false,
            true,
            true,
            true
        ));
        assert!(installed_flag("provider", false, false, true, false, false));
        assert!(!installed_flag("provider", true, true, false, false, false));
        assert!(installed_flag("gateway", false, false, false, true, false));
        assert!(!installed_flag("gateway", true, false, false, false, false));
        assert!(installed_flag(
            "skill-pack",
            false,
            false,
            false,
            false,
            true
        ));
        assert!(!installed_flag(
            "skill-pack",
            true,
            true,
            false,
            false,
            false
        ));
    }

    #[test]
    fn provider_family_falls_back_to_id() {
        assert_eq!(provider_family("anthropic-oauth"), "anthropic");
        assert_eq!(provider_family("not-a-provider"), "not-a-provider");
    }

    // ---------- compute_installed: settings-less gateway ----------

    #[tokio::test]
    async fn compute_installed_gateway_without_settings_follows_enabled() {
        // A gateway with no manifest settings has nothing to configure, so its
        // installed-ness must track `enabled` — otherwise it could never leave
        // Browse. `gateway_only` builds a manifest with empty `settings`.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = ryuzi_core::Store::open(tmp.path()).await.unwrap();
        let plugin = gateway_only("bare-gateway");
        let ctx = InstalledCtx {
            connections: vec![],
            installed_skills: vec![],
        };

        let installed_when_enabled =
            compute_installed(&store, &plugin, "gateway", true, false, &ctx)
                .await
                .unwrap();
        assert!(
            installed_when_enabled,
            "enabled settings-less gateway is installed"
        );

        let installed_when_disabled =
            compute_installed(&store, &plugin, "gateway", false, false, &ctx)
                .await
                .unwrap();
        assert!(
            !installed_when_disabled,
            "disabled settings-less gateway is not installed"
        );
    }

    // ---------- plugin_info ----------

    #[test]
    fn plugin_info_maps_identity_and_enabled_flag_through() {
        let plugin = harness_only("native");
        let info = plugin_info(&plugin, true, false, "integration", false, None);
        assert_eq!(info.id, "native");
        assert_eq!(info.name, "Plugin native");
        assert!(info.enabled);
        assert_eq!(info.source, "builtin");
        assert_eq!(info.capabilities, vec!["runtime".to_string()]);
        assert!(!info.configured);
        assert_eq!(info.kind, "integration");
        assert!(!info.installed);
        assert!(info.family.is_none());
        // No ledger row: pinned/sourceSpec/etc default to the "never
        // installed via the tracked git-clone path" shape.
        assert!(!info.pinned);
        assert!(info.source_spec.is_none());
        assert!(info.resolved_commit.is_none());
        assert!(info.installed_at.is_none());
        assert!(info.updated_at.is_none());
        assert!(info.trust_tier.is_none());

        let info_disabled = plugin_info(&plugin, false, false, "integration", false, None);
        assert!(!info_disabled.enabled);
    }

    #[test]
    fn plugin_info_carries_ledger_fields_when_a_record_is_present() {
        let plugin = harness_only("native");
        let record = ryuzi_core::store::PluginInstallRecord {
            plugin_id: "native".to_string(),
            kind: "plugin_pack".to_string(),
            source_spec: "https://github.com/acme/native-pack".to_string(),
            resolved_commit: Some("abc123".to_string()),
            fingerprint: "sha256:deadbeef".to_string(),
            installed_at: 1_700_000_000,
            updated_at: 1_700_000_500,
            pinned: true,
            pin_reason: Some("manual pin".to_string()),
            trust_tier: "acknowledged".to_string(),
            trust_ack_at: Some(1_700_000_000),
            trust_ack_summary: None,
        };
        let info = plugin_info(&plugin, true, false, "integration", false, Some(&record));
        assert!(info.pinned);
        assert_eq!(
            info.source_spec.as_deref(),
            Some("https://github.com/acme/native-pack")
        );
        assert_eq!(info.resolved_commit.as_deref(), Some("abc123"));
        assert_eq!(info.installed_at, Some(1_700_000_000));
        assert_eq!(info.updated_at, Some(1_700_000_500));
        assert_eq!(info.trust_tier.as_deref(), Some("acknowledged"));
    }

    #[tokio::test]
    async fn assemble_list_excludes_runtimes_and_synthesizes_curated_packs() {
        let cp = test_cp().await;
        let list = assemble_list(&cp).await.unwrap();
        assert!(list
            .iter()
            .all(|p| p.id != "native" && p.id != "claude-code"));
        assert!(list
            .iter()
            .any(|p| p.kind == "skill-pack" && p.id == "superpowers"));
        let discord = list.iter().find(|p| p.id == "discord").expect("discord");
        assert_eq!(discord.kind, "gateway");
        assert!(!discord.installed, "no discord settings persisted yet");
        let anthropic = list.iter().find(|p| p.id == "anthropic").expect("provider");
        assert_eq!(anthropic.kind, "provider");
        assert_eq!(anthropic.family.as_deref(), Some("anthropic"));
    }

    /// `assemble_list`/`assemble_detail` fetch `plugin_installs` once and
    /// index by plugin id (`install_ledger_index`) rather than a per-plugin
    /// `get_plugin_install` — this is the end-to-end (ControlPlane-backed)
    /// regression guard for that enrichment, mirroring `serve.rs`'s
    /// `get_plugin_enrichment_keeps_source_enum_and_adds_source_spec`.
    #[tokio::test]
    async fn assemble_list_and_detail_carry_install_ledger_fields() {
        let cp = test_cp().await;
        cp.store()
            .upsert_plugin_install(&ryuzi_core::store::PluginInstallRecord {
                plugin_id: "anthropic".to_string(),
                kind: "plugin_pack".to_string(),
                source_spec: "https://github.com/acme/anthropic-pack".to_string(),
                resolved_commit: Some("abc123".to_string()),
                fingerprint: "sha256:deadbeef".to_string(),
                installed_at: 1_700_000_000,
                updated_at: 1_700_000_500,
                pinned: true,
                pin_reason: Some("manual pin".to_string()),
                trust_tier: "acknowledged".to_string(),
                trust_ack_at: Some(1_700_000_000),
                trust_ack_summary: None,
            })
            .await
            .unwrap();

        let list = assemble_list(&cp).await.unwrap();
        let anthropic = list.iter().find(|p| p.id == "anthropic").unwrap();
        assert!(anthropic.pinned);
        assert_eq!(
            anthropic.source_spec.as_deref(),
            Some("https://github.com/acme/anthropic-pack")
        );
        assert_eq!(anthropic.resolved_commit.as_deref(), Some("abc123"));
        assert_eq!(anthropic.installed_at, Some(1_700_000_000));
        assert_eq!(anthropic.updated_at, Some(1_700_000_500));
        assert_eq!(anthropic.trust_tier.as_deref(), Some("acknowledged"));

        // A plugin with no ledger row stays at the "never installed via the
        // tracked git-clone path" defaults.
        let notion = list.iter().find(|p| p.id == "notion").unwrap();
        assert!(!notion.pinned);
        assert!(notion.source_spec.is_none());
        assert!(notion.trust_tier.is_none());

        let detail = assemble_detail(&cp, "anthropic").await.unwrap();
        assert!(detail.info.pinned);
        assert_eq!(
            detail.info.source_spec.as_deref(),
            Some("https://github.com/acme/anthropic-pack")
        );
        assert_eq!(detail.info.trust_tier.as_deref(), Some("acknowledged"));

        let notion_detail = assemble_detail(&cp, "notion").await.unwrap();
        assert!(!notion_detail.info.pinned);
        assert!(notion_detail.info.source_spec.is_none());
    }

    // ---------- auth_kind_label / auth_configured ----------

    #[test]
    fn auth_kind_label_maps_every_variant() {
        assert_eq!(auth_kind_label(AuthKind::None), "none");
        assert_eq!(auth_kind_label(AuthKind::ApiKey), "api-key");
        assert_eq!(auth_kind_label(AuthKind::Token), "token");
        assert_eq!(auth_kind_label(AuthKind::Oauth), "oauth");
    }

    #[test]
    fn auth_configured_true_when_setting_value_is_non_empty() {
        assert!(auth_configured(Some("sk-secret"), false));
    }

    #[test]
    fn auth_configured_true_when_env_fallback_is_set() {
        assert!(auth_configured(None, true));
        assert!(auth_configured(Some(""), true));
    }

    #[test]
    fn auth_configured_false_when_neither_setting_nor_env_present() {
        assert!(!auth_configured(None, false));
        assert!(!auth_configured(Some(""), false));
    }

    #[tokio::test]
    async fn plugin_oauth_authorize_url_uses_pkce_scopes_and_client_id_from_settings() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = ryuzi_core::Store::open(tmp.path()).await.unwrap();
        store
            .set_setting_raw("plugin.acme.client_id", "acme-client-123")
            .await
            .unwrap();

        let auth = AuthSpec {
            kind: AuthKind::Oauth,
            authorize_url: Some("https://acme.example.com/oauth/authorize".into()),
            token_url: Some("https://acme.example.com/oauth/token".into()),
            scopes: vec!["repo".into(), "issues:read".into()],
            client_id_setting: Some("plugin.acme.client_id".into()),
            extra_authorize_params: BTreeMap::from([("prompt".into(), "consent".into())]),
            ..Default::default()
        };

        let begin = build_plugin_oauth_begin_result(
            &store,
            "acme-oauth",
            &auth,
            "verifier-test-123",
            "state-test-123",
        )
        .await
        .unwrap();

        let url = reqwest::Url::parse(&begin.authorize_url).unwrap();
        let query: BTreeMap<String, String> = url.query_pairs().into_owned().collect();
        assert_eq!(
            url.as_str().split('?').next().unwrap(),
            "https://acme.example.com/oauth/authorize"
        );
        assert_eq!(
            query.get("client_id").map(String::as_str),
            Some("acme-client-123")
        );
        assert_eq!(
            query.get("code_challenge").map(String::as_str),
            Some(ryuzi_core::plugins::oauth::pkce_challenge_s256("verifier-test-123").as_str())
        );
        assert_eq!(
            query.get("code_challenge_method").map(String::as_str),
            Some("S256")
        );
        assert_eq!(query.get("response_type").map(String::as_str), Some("code"));
        assert_eq!(
            query.get("state").map(String::as_str),
            Some("state-test-123")
        );
        assert_eq!(
            query.get("scope").map(String::as_str),
            Some("repo issues:read")
        );
        assert_eq!(query.get("prompt").map(String::as_str), Some("consent"));
        assert_eq!(
            query.get("redirect_uri").map(String::as_str),
            Some(begin.redirect_uri.as_str())
        );
    }

    #[tokio::test]
    async fn resolve_plugin_oauth_orders_row_then_setting_then_external_auth_setting() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = ryuzi_core::Store::open(tmp.path()).await.unwrap();
        store
            .set_setting_raw("plugin.gw.client_id", "setting-client")
            .await
            .unwrap();

        // External plugin (no resource, no authorize_url): auth.setting IS
        // the client id key (google-workspace shape).
        let external = AuthSpec {
            kind: AuthKind::Oauth,
            setting: Some("plugin.gw.client_id".into()),
            ..Default::default()
        };
        assert!(is_external_oauth(&external));
        let resolved = resolve_plugin_oauth(&store, "gw", &external).await.unwrap();
        assert_eq!(resolved.client_id.as_deref(), Some("setting-client"));

        // Non-external (resource declared): auth.setting is NOT consulted.
        let non_external = AuthSpec {
            kind: AuthKind::Oauth,
            setting: Some("plugin.gw.client_id".into()),
            resource: Some("https://vendor.test/mcp".into()),
            ..Default::default()
        };
        assert!(!is_external_oauth(&non_external));
        let resolved = resolve_plugin_oauth(&store, "gw", &non_external)
            .await
            .unwrap();
        assert_eq!(resolved.client_id, None);

        // client_id_setting is second in the order…
        let with_setting = AuthSpec {
            client_id_setting: Some("plugin.gw.client_id".into()),
            ..non_external.clone()
        };
        let resolved = resolve_plugin_oauth(&store, "gw", &with_setting)
            .await
            .unwrap();
        assert_eq!(resolved.client_id.as_deref(), Some("setting-client"));

        // …and the plugin_oauth_clients row wins over everything, endpoints
        // included (table → manifest).
        store
            .upsert_plugin_oauth_client(&PluginOauthClient {
                plugin_id: "gw".into(),
                authorize_url: Some("https://discovered.test/authorize".into()),
                token_url: Some("https://discovered.test/token".into()),
                client_id: Some("row-client".into()),
            })
            .await
            .unwrap();
        let resolved = resolve_plugin_oauth(&store, "gw", &with_setting)
            .await
            .unwrap();
        assert_eq!(resolved.client_id.as_deref(), Some("row-client"));
        assert_eq!(
            resolved.authorize_url.as_deref(),
            Some("https://discovered.test/authorize")
        );
        assert_eq!(
            resolved.token_url.as_deref(),
            Some("https://discovered.test/token")
        );
    }

    #[tokio::test]
    async fn begin_result_prefers_table_endpoints_over_manifest() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = ryuzi_core::Store::open(tmp.path()).await.unwrap();
        store
            .upsert_plugin_oauth_client(&PluginOauthClient {
                plugin_id: "acme-table".into(),
                authorize_url: Some("https://discovered.test/authorize".into()),
                token_url: Some("https://discovered.test/token".into()),
                client_id: Some("row-client".into()),
            })
            .await
            .unwrap();
        let auth = AuthSpec {
            kind: AuthKind::Oauth,
            authorize_url: Some("https://manifest.test/authorize".into()),
            token_url: Some("https://manifest.test/token".into()),
            ..Default::default()
        };
        let begin = build_plugin_oauth_begin_result(&store, "acme-table", &auth, "v-1", "s-1")
            .await
            .unwrap();
        assert!(
            begin
                .authorize_url
                .starts_with("https://discovered.test/authorize?"),
            "{}",
            begin.authorize_url
        );
        assert!(begin.authorize_url.contains("client_id=row-client"));
    }

    // ---------- field_value_set ----------

    #[test]
    fn field_value_set_true_only_for_non_empty_persisted_value() {
        assert!(field_value_set(Some("x")));
        assert!(!field_value_set(Some("")));
        assert!(!field_value_set(None));
    }

    // ---------- mcp_transport_label / mcp_info ----------

    #[test]
    fn mcp_transport_label_maps_both_variants() {
        assert_eq!(mcp_transport_label(McpTransportDef::Stdio), "stdio");
        assert_eq!(mcp_transport_label(McpTransportDef::Http), "http");
    }

    #[test]
    fn mcp_info_uses_command_for_stdio_and_url_for_http() {
        let stdio = McpServerDef {
            name: "svc".to_string(),
            transport: McpTransportDef::Stdio,
            command: Some("npx".to_string()),
            args: vec![],
            env: Default::default(),
            url: None,
            headers: Default::default(),
        };
        let info = mcp_info(&stdio);
        assert_eq!(info.transport, "stdio");
        assert_eq!(info.command_or_url, "npx");

        let http = McpServerDef {
            name: "svc2".to_string(),
            transport: McpTransportDef::Http,
            command: None,
            args: vec![],
            env: Default::default(),
            url: Some("https://example.com/mcp".to_string()),
            headers: Default::default(),
        };
        let info2 = mcp_info(&http);
        assert_eq!(info2.transport, "http");
        assert_eq!(info2.command_or_url, "https://example.com/mcp");
    }

    // ---------- assemble_list / assemble_detail (ControlPlane-backed) ----------

    async fn test_cp() -> Arc<ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = ryuzi_core::Store::open(tmp.path()).await.unwrap();
        let mut regs = Registries::new();
        ryuzi_core::plugins::install_builtins(&mut regs);
        ControlPlane::new(store, regs).await
    }

    #[tokio::test]
    async fn list_includes_anthropic_enabled_with_provider_capability() {
        let cp = test_cp().await;
        let list = assemble_list(&cp).await.unwrap();
        let anthropic = list
            .iter()
            .find(|p| p.id == "anthropic")
            .expect("anthropic plugin present");
        assert!(
            anthropic.enabled,
            "manifest-only plugins are always enabled"
        );
        assert_eq!(anthropic.capabilities, vec!["provider".to_string()]);
        assert_eq!(anthropic.source, "builtin");
    }

    #[tokio::test]
    async fn detail_unknown_id_errors() {
        let cp = test_cp().await;
        match assemble_detail(&cp, "nope").await {
            Ok(_) => panic!("expected an error for an unknown plugin id"),
            Err(e) => assert_eq!(e.to_string(), "unknown plugin: nope"),
        }
    }

    #[tokio::test]
    async fn detail_anthropic_has_provider_models_and_unconfigured_api_key_auth() {
        let cp = test_cp().await;
        let detail = assemble_detail(&cp, "anthropic").await.unwrap();
        assert_eq!(detail.info.id, "anthropic");
        assert!(!detail.models.is_empty());
        assert!(detail.settings.is_empty());
        assert!(detail.mcp.is_empty());
        assert_eq!(detail.publisher, "ryuzi");

        let auth = detail
            .auth
            .expect("anthropic manifest declares an auth block");
        assert_eq!(auth.kind, "api-key");
        assert!(
            !auth.configured,
            "no connection/env configured in a fresh store"
        );
    }

    /// Same seam as core's `secrets::use_test_key_file` (pub(crate) there;
    /// separate crates keep their own copy): point the process-global cipher
    /// at a throwaway key file BEFORE the first encrypt/decrypt so tests
    /// never touch the OS keychain or a real secret.key.
    fn use_cockpit_test_key_file() {
        static INIT: std::sync::Once = std::sync::Once::new();
        INIT.call_once(|| {
            let path = std::env::temp_dir().join(format!(
                "ryuzi-cockpit-test-secret-{}.key",
                std::process::id()
            ));
            std::env::set_var("RYUZI_SECRET_KEY_FILE", path);
        });
    }

    #[tokio::test]
    async fn plugin_info_configured_matches_auth_info_semantics_for_non_oauth() {
        let cp = test_cp().await;
        let list = assemble_list(&cp).await.unwrap();
        let anthropic = list.iter().find(|p| p.id == "anthropic").unwrap();
        assert!(!anthropic.configured, "fresh store: nothing configured");
        let detail = assemble_detail(&cp, "anthropic").await.unwrap();
        assert_eq!(
            detail.info.configured,
            detail.auth.expect("anthropic declares auth").configured
        );
    }

    #[tokio::test]
    async fn plugin_info_configured_for_oauth_requires_stored_token_without_reconnect() {
        use_cockpit_test_key_file();
        let cp = test_cp().await;
        // notion is a catalog kind=oauth plugin.
        let before = assemble_detail(&cp, "notion").await.unwrap();
        assert!(!before.info.configured);

        cp.store()
            .upsert_plugin_oauth_token(&PluginOauthToken {
                plugin_id: "notion".into(),
                access_token: "tok".into(),
                refresh_token: None,
                token_type: "Bearer".into(),
                expires_at: None,
                scopes: vec![],
                reconnect_required: false,
            })
            .await
            .unwrap();
        let with_token = assemble_detail(&cp, "notion").await.unwrap();
        assert!(with_token.info.configured);

        cp.store()
            .mark_plugin_oauth_reconnect_required("notion")
            .await
            .unwrap();
        let reconnect = assemble_detail(&cp, "notion").await.unwrap();
        assert!(
            !reconnect.info.configured,
            "reconnect_required must unset configured"
        );
    }

    #[tokio::test]
    async fn set_plugin_enabled_and_setting_round_trip_through_the_control_plane() {
        let cp = test_cp().await;
        let settings = SettingsStore::new(cp.store().clone());

        // anthropic is a manifest-only plugin (no harness/gateway/connector
        // capability): `is_enabled` always reports it enabled regardless of
        // any `plugin.<id>.enabled` write, so toggling it must error rather
        // than silently no-op (see `toggle_enabled`'s doc).
        let err = ryuzi_core::plugins::toggle_enabled(cp.plugins(), &settings, "anthropic", true)
            .await
            .unwrap_err();
        assert_eq!(err.to_string(), "anthropic is always available");

        settings
            .set("default_perm_mode", "acceptEdits")
            .await
            .unwrap();
        assert_eq!(
            settings.get("default_perm_mode").await.unwrap().as_deref(),
            Some("acceptEdits")
        );
    }

    // ---------- uninstall (kind-symmetric teardown) ----------

    #[tokio::test]
    async fn uninstall_gateway_clears_settings_and_disables() {
        let cp = test_cp().await;
        let settings = SettingsStore::new(cp.store().clone());
        cp.store()
            .set_setting_raw("discord.token", "t")
            .await
            .unwrap();
        cp.store()
            .set_setting_raw("discord.app_id", "a")
            .await
            .unwrap();
        cp.store()
            .set_setting_raw("discord.guild_id", "g")
            .await
            .unwrap();
        ryuzi_core::plugins::toggle_enabled(cp.plugins(), &settings, "discord", true)
            .await
            .unwrap();

        uninstall(&cp, "discord").await.unwrap();

        assert_eq!(
            cp.store().get_setting_raw("discord.token").await.unwrap(),
            None
        );
        assert_eq!(
            cp.store().get_setting_raw("discord.app_id").await.unwrap(),
            None
        );
        assert_eq!(
            cp.store()
                .get_setting_raw("discord.guild_id")
                .await
                .unwrap(),
            None
        );
        assert!(!cp.plugins().is_enabled(&settings, "discord").await.unwrap());
    }

    #[tokio::test]
    async fn uninstall_provider_removes_every_family_connection() {
        let cp = test_cp().await;
        let now = ryuzi_core::paths::now_ms();
        for (id, provider) in [
            ("c1", "anthropic"),
            ("c2", "anthropic-oauth"),
            ("c3", "openai"),
        ] {
            ryuzi_core::llm_router::connections::add_connection(
                cp.store(),
                ryuzi_core::llm_router::connections::ConnectionRow {
                    id: id.into(),
                    provider: provider.into(),
                    auth_type: "api_key".into(),
                    label: id.into(),
                    priority: 0,
                    enabled: true,
                    data: Default::default(),
                    created_at: now,
                    updated_at: now,
                },
            )
            .await
            .unwrap();
        }

        uninstall(&cp, "anthropic").await.unwrap();

        let left = ryuzi_core::llm_router::connections::list_connections(cp.store())
            .await
            .unwrap();
        let providers: Vec<_> = left.iter().map(|c| c.provider.as_str()).collect();
        assert_eq!(
            providers,
            vec!["openai"],
            "family (anthropic + anthropic-oauth) removed"
        );
    }

    #[tokio::test]
    async fn uninstall_integration_clears_credential_and_disables() {
        let cp = test_cp().await;
        cp.store()
            .set_setting_raw("plugin.github.token", "tok")
            .await
            .unwrap();
        let settings = SettingsStore::new(cp.store().clone());
        ryuzi_core::plugins::toggle_enabled(cp.plugins(), &settings, "github", true)
            .await
            .unwrap();

        uninstall(&cp, "github").await.unwrap();

        assert_eq!(
            cp.store()
                .get_setting_raw("plugin.github.token")
                .await
                .unwrap(),
            None
        );
        assert!(!cp.plugins().is_enabled(&settings, "github").await.unwrap());
    }

    #[tokio::test]
    async fn uninstall_unknown_id_errors() {
        let cp = test_cp().await;
        assert!(uninstall(&cp, "definitely-not-a-plugin").await.is_err());
    }

    // The positive skill-pack uninstall path (a real pack on disk being
    // removed via `remove_installed_skill`) is deliberately NOT exercised
    // here: it requires the `ryuzi_core::skills_install` install seam
    // (`InstallRoots`, `install_skill_source_with`) which is private to the
    // `ryuzi-core` crate and unreachable from `ryuzi-cockpit`, and the public
    // API resolves through `InstallRoots::for_user()`, i.e. the real user
    // skills install dir — network/git-touching and environment-dependent.
    // That path is covered by `ryuzi_core::skills_install` unit tests plus the
    // frontend `PluginsView` uninstall test. The cockpit-level tests below
    // assert only the hermetic, deterministic bail paths.

    #[tokio::test]
    async fn uninstall_skill_pack_unknown_id_errors() {
        // No skill packs installed and the id is not a registered plugin, so
        // the not-in-host fallback resolves through `list_installed_skills()`,
        // finds nothing, and bails.
        let cp = test_cp().await;
        assert!(uninstall(&cp, "definitely-not-installed-pack")
            .await
            .is_err());
    }

    // `uninstall_plugin` only runs `assemble_list` after a successful
    // `uninstall`, so the unknown-id bail above propagates unchanged through
    // the command wrapper via the `?` on `uninstall(...).await`. The wrapper
    // itself takes a `tauri::State`, which has no hermetic constructor outside
    // a running Tauri app, so it is not driven directly here.

    // ---------- begin_plugin_install resolution (steps 1-6) ----------

    /// Minimal hand-rolled HTTP mock on std::net (this crate has no axum —
    /// core's axum-mock convention doesn't apply here; adding axum would be
    /// a new dependency). Serves the RFC 8414 root + path-inserted documents
    /// (both spellings share the well-known prefix) pointing endpoints (and,
    /// when `with_registration`, the registration endpoint) at itself, plus
    /// an RFC 7591 register endpoint; counts hits per route.
    fn spawn_mock_vendor(
        with_registration: bool,
        discovery_hits: Arc<AtomicUsize>,
        register_hits: Arc<AtomicUsize>,
    ) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        let served_base = base.clone();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let Ok(mut stream) = stream else { continue };
                let base = served_base.clone();
                let discovery_hits = discovery_hits.clone();
                let register_hits = register_hits.clone();
                std::thread::spawn(move || {
                    let mut buf = Vec::new();
                    let mut chunk = [0u8; 1024];
                    let header_end = loop {
                        if let Some(pos) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
                            break pos + 4;
                        }
                        match stream.read(&mut chunk) {
                            Ok(0) | Err(_) => return,
                            Ok(n) => buf.extend_from_slice(&chunk[..n]),
                        }
                    };
                    let head = String::from_utf8_lossy(&buf[..header_end]).into_owned();
                    // Drain any request body so the client never sees a
                    // reset while still writing.
                    if let Some(len) = head.lines().find_map(|line| {
                        line.to_ascii_lowercase()
                            .strip_prefix("content-length:")
                            .and_then(|v| v.trim().parse::<usize>().ok())
                    }) {
                        let mut have = buf.len() - header_end;
                        while have < len {
                            match stream.read(&mut chunk) {
                                Ok(0) | Err(_) => break,
                                Ok(n) => have += n,
                            }
                        }
                    }
                    let request_line = head.lines().next().unwrap_or_default().to_string();
                    let (status, body) = if request_line
                        .starts_with("GET /.well-known/oauth-authorization-server")
                    {
                        discovery_hits.fetch_add(1, Ordering::SeqCst);
                        let registration = if with_registration {
                            format!(r#","registration_endpoint":"{base}/register""#)
                        } else {
                            String::new()
                        };
                        (
                            "200 OK",
                            format!(
                                r#"{{"authorization_endpoint":"{base}/authorize","token_endpoint":"{base}/token"{registration}}}"#
                            ),
                        )
                    } else if request_line.starts_with("POST /register") {
                        register_hits.fetch_add(1, Ordering::SeqCst);
                        ("200 OK", r#"{"client_id":"dcr-client-123"}"#.to_string())
                    } else {
                        ("404 Not Found", String::new())
                    };
                    let response = format!(
                        "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{}",
                        body.len(),
                        body
                    );
                    let _ = stream.write_all(response.as_bytes());
                    let _ = stream.flush();
                });
            }
        });
        base
    }

    async fn test_store() -> (ryuzi_core::Store, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = ryuzi_core::Store::open(tmp.path()).await.unwrap();
        (store, tmp)
    }

    #[tokio::test]
    async fn begin_env_var_short_circuits_before_any_oauth_work() {
        let (store, _tmp) = test_store().await;
        let var = "RYUZI_TEST_WIZ_ENV_7a91";
        std::env::set_var(var, "present");
        let auth = AuthSpec {
            kind: AuthKind::ApiKey,
            env: Some(var.to_string()),
            ..Default::default()
        };
        let http = reqwest::Client::new();
        let result = resolve_plugin_install(&store, &http, "wiz-env", Some(&auth))
            .await
            .unwrap();
        assert_eq!(result.auth_kind, "api-key");
        assert!(result.env_var_present);
        assert_eq!(result.env_var_name.as_deref(), Some(var));
        assert!(result.oauth_begin.is_none());
        std::env::remove_var(var);
    }

    #[tokio::test]
    async fn begin_non_oauth_kind_reports_kind_only() {
        let (store, _tmp) = test_store().await;
        let auth = AuthSpec {
            kind: AuthKind::Token,
            setting: Some("plugin.wiz-token.token".into()),
            ..Default::default()
        };
        let http = reqwest::Client::new();
        let result = resolve_plugin_install(&store, &http, "wiz-token", Some(&auth))
            .await
            .unwrap();
        assert_eq!(result.auth_kind, "token");
        assert!(!result.env_var_present);
        assert!(!result.oauth_available && !result.oauth_external && !result.needs_client_id);
        // And no [auth] block at all behaves as "none".
        let result = resolve_plugin_install(&store, &http, "wiz-none", None)
            .await
            .unwrap();
        assert_eq!(result.auth_kind, "none");
    }

    #[tokio::test]
    async fn begin_external_oauth_never_discovers_and_tracks_saved_client_id() {
        let (store, _tmp) = test_store().await;
        let auth = AuthSpec {
            kind: AuthKind::Oauth,
            setting: Some("plugin.wiz-external.client_id".into()),
            ..Default::default()
        };
        let http = reqwest::Client::new();
        let result = resolve_plugin_install(&store, &http, "wiz-external", Some(&auth))
            .await
            .unwrap();
        assert!(result.oauth_external);
        assert!(result.needs_client_id, "no saved auth.setting value yet");
        assert!(!result.oauth_available);
        assert!(result.oauth_begin.is_none());

        store
            .set_setting_raw("plugin.wiz-external.client_id", "google-client")
            .await
            .unwrap();
        let result = resolve_plugin_install(&store, &http, "wiz-external", Some(&auth))
            .await
            .unwrap();
        assert!(result.oauth_external);
        assert!(!result.needs_client_id);
        assert!(
            result.oauth_begin.is_none(),
            "external never opens a browser"
        );
    }

    #[tokio::test]
    async fn begin_runs_discovery_then_dcr_then_reuses_the_cache() {
        let (store, _tmp) = test_store().await;
        let discovery_hits = Arc::new(AtomicUsize::new(0));
        let register_hits = Arc::new(AtomicUsize::new(0));
        let base = spawn_mock_vendor(true, discovery_hits.clone(), register_hits.clone());
        let auth = AuthSpec {
            kind: AuthKind::Oauth,
            resource: Some(format!("{base}/mcp")),
            dynamic_registration: true,
            ..Default::default()
        };
        let http = reqwest::Client::new();

        let result = resolve_plugin_install(&store, &http, "wiz-dcr", Some(&auth))
            .await
            .unwrap();
        assert!(result.dcr_succeeded);
        assert!(result.oauth_available);
        assert!(!result.needs_client_id);
        let begin = result.oauth_begin.expect("browser flow prepared");
        assert!(
            begin
                .authorize_url
                .starts_with(&format!("{base}/authorize?")),
            "{}",
            begin.authorize_url
        );
        assert!(begin.authorize_url.contains("client_id=dcr-client-123"));
        assert_eq!(discovery_hits.load(Ordering::SeqCst), 1);
        assert_eq!(register_hits.load(Ordering::SeqCst), 1);
        // Flow state was stored for the callback/exchange.
        assert!(plugin_oauth_flows()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .contains_key(&plugin_oauth_flow_key("wiz-dcr", &begin.state_token)));

        // Endpoints + client id persisted.
        let row = store
            .get_plugin_oauth_client("wiz-dcr")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.authorize_url.as_deref(),
            Some(format!("{base}/authorize").as_str())
        );
        assert_eq!(
            row.token_url.as_deref(),
            Some(format!("{base}/token").as_str())
        );
        assert_eq!(row.client_id.as_deref(), Some("dcr-client-123"));

        // Second begin: cached endpoints reused (no second discovery) and a
        // client id on the row permanently suppresses DCR.
        let result2 = resolve_plugin_install(&store, &http, "wiz-dcr", Some(&auth))
            .await
            .unwrap();
        assert!(result2.oauth_available);
        assert!(!result2.dcr_succeeded);
        assert_eq!(
            discovery_hits.load(Ordering::SeqCst),
            1,
            "no second discovery"
        );
        assert_eq!(
            register_hits.load(Ordering::SeqCst),
            1,
            "no second registration"
        );
    }

    #[tokio::test]
    async fn begin_without_registration_endpoint_persists_endpoints_then_manual_id_skips_dcr() {
        let (store, _tmp) = test_store().await;
        let discovery_hits = Arc::new(AtomicUsize::new(0));
        let register_hits = Arc::new(AtomicUsize::new(0));
        // Slack shape: endpoints discoverable, registration closed, manifest
        // does not opt into dynamic-registration.
        let base = spawn_mock_vendor(false, discovery_hits.clone(), register_hits.clone());
        let auth = AuthSpec {
            kind: AuthKind::Oauth,
            resource: Some(format!("{base}/mcp")),
            ..Default::default()
        };
        let http = reqwest::Client::new();

        let result = resolve_plugin_install(&store, &http, "wiz-slack", Some(&auth))
            .await
            .unwrap();
        assert!(result.needs_client_id);
        assert!(!result.oauth_available);
        assert!(!result.dcr_succeeded);
        assert_eq!(register_hits.load(Ordering::SeqCst), 0);
        // Endpoints survive even though registration is impossible.
        let row = store
            .get_plugin_oauth_client("wiz-slack")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            row.token_url.as_deref(),
            Some(format!("{base}/token").as_str())
        );
        assert!(row.client_id.is_none());

        // Manual client id → re-begin goes straight to the browser flow.
        store
            .upsert_plugin_oauth_client(&PluginOauthClient {
                plugin_id: "wiz-slack".into(),
                authorize_url: None,
                token_url: None,
                client_id: Some("manual-client".into()),
            })
            .await
            .unwrap();
        let result = resolve_plugin_install(&store, &http, "wiz-slack", Some(&auth))
            .await
            .unwrap();
        assert!(result.oauth_available);
        assert!(!result.needs_client_id);
        assert!(result
            .oauth_begin
            .unwrap()
            .authorize_url
            .contains("client_id=manual-client"));
        assert_eq!(
            discovery_hits.load(Ordering::SeqCst),
            1,
            "cached endpoints reused"
        );
        assert_eq!(
            register_hits.load(Ordering::SeqCst),
            0,
            "DCR never attempted"
        );
    }

    #[tokio::test]
    async fn begin_discovery_failure_with_no_endpoints_reports_only_the_error() {
        let (store, _tmp) = test_store().await;
        // Bind then drop: requests to this port are refused.
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        drop(listener);
        let auth = AuthSpec {
            kind: AuthKind::Oauth,
            resource: Some(format!("{base}/mcp")),
            dynamic_registration: true,
            ..Default::default()
        };
        let http = reqwest::Client::new();
        let result = resolve_plugin_install(&store, &http, "wiz-down", Some(&auth))
            .await
            .unwrap();
        assert!(!result.oauth_available);
        assert!(
            !result.needs_client_id,
            "nothing to enter without endpoints"
        );
        assert!(result.dcr_error.is_some());
        assert!(result.oauth_begin.is_none());
    }

    #[tokio::test]
    async fn set_plugin_oauth_client_id_routes_external_to_auth_setting_and_others_to_the_row() {
        let cp = test_cp().await;
        // google-workspace is the external-OAuth catalog plugin — its client
        // id key IS its auth.setting (validated write path).
        set_plugin_oauth_client_id_inner(&cp, "google-workspace", " google-client-1 ")
            .await
            .unwrap();
        assert_eq!(
            cp.store()
                .get_setting_raw("plugin.google-workspace.client_id")
                .await
                .unwrap()
                .as_deref(),
            Some("google-client-1"),
            "trimmed value stored under the declared auth.setting"
        );
        assert!(
            cp.store()
                .get_plugin_oauth_client("google-workspace")
                .await
                .unwrap()
                .is_none(),
            "external plugins never write the row"
        );

        // notion (resource-declared oauth) goes to plugin_oauth_clients.
        set_plugin_oauth_client_id_inner(&cp, "notion", "notion-client-1")
            .await
            .unwrap();
        let row = cp
            .store()
            .get_plugin_oauth_client("notion")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.client_id.as_deref(), Some("notion-client-1"));
        assert!(row.authorize_url.is_none());

        // Empty input is rejected.
        assert!(set_plugin_oauth_client_id_inner(&cp, "notion", "  ")
            .await
            .is_err());
    }

    #[tokio::test]
    async fn cancel_pending_plugin_flows_narrows_by_state_token_or_sweeps_the_plugin() {
        let insert = |token: &str| {
            plugin_oauth_flows()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .insert(
                    plugin_oauth_flow_key("wiz-cancel", token),
                    PluginOauthFlowState {
                        verifier: "v".into(),
                        redirect_uri: plugin_oauth_redirect_uri("wiz-cancel"),
                        requested_scopes: vec![],
                    },
                );
        };
        insert("s1");
        insert("s2");
        cancel_pending_plugin_flows("wiz-cancel", Some("s1"));
        {
            let flows = plugin_oauth_flows()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            assert!(!flows.contains_key(&plugin_oauth_flow_key("wiz-cancel", "s1")));
            assert!(flows.contains_key(&plugin_oauth_flow_key("wiz-cancel", "s2")));
        }
        cancel_pending_plugin_flows("wiz-cancel", None);
        let flows = plugin_oauth_flows()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert!(!flows.contains_key(&plugin_oauth_flow_key("wiz-cancel", "s2")));
    }

    // ---------- finish_plugin_oauth_callback ----------

    fn insert_test_flow(plugin_id: &str, state_token: &str, verifier: &str) -> String {
        let flow_key = plugin_oauth_flow_key(plugin_id, state_token);
        plugin_oauth_flows()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .insert(
                flow_key.clone(),
                PluginOauthFlowState {
                    verifier: verifier.to_string(),
                    redirect_uri: plugin_oauth_redirect_uri(plugin_id),
                    requested_scopes: vec![],
                },
            );
        flow_key
    }

    #[tokio::test]
    async fn finish_callback_missing_code_errors_and_preserves_the_flow() {
        let (store, _tmp) = test_store().await;
        let flow_key = insert_test_flow("wiz-cb-code", "state-a", "verifier-a");
        let auth = AuthSpec {
            kind: AuthKind::Oauth,
            ..Default::default()
        };
        let callback = ryuzi_core::oauth_loopback::CallbackResult {
            code: None,
            state: Some("state-a".into()),
        };
        let err = finish_plugin_oauth_callback(&store, "wiz-cb-code", &auth, "state-a", callback)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("code"), "{err}");
        assert!(
            plugin_oauth_flows()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .contains_key(&flow_key),
            "a missing-code callback must leave the flow entry in place for retry"
        );
    }

    #[tokio::test]
    async fn finish_callback_missing_state_errors_and_preserves_the_flow() {
        let (store, _tmp) = test_store().await;
        let flow_key = insert_test_flow("wiz-cb-state", "state-b", "verifier-b");
        let auth = AuthSpec {
            kind: AuthKind::Oauth,
            ..Default::default()
        };
        let callback = ryuzi_core::oauth_loopback::CallbackResult {
            code: Some("auth-code".into()),
            state: None,
        };
        let err = finish_plugin_oauth_callback(&store, "wiz-cb-state", &auth, "state-b", callback)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("state"), "{err}");
        assert!(
            plugin_oauth_flows()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .contains_key(&flow_key),
            "a missing-state callback must leave the flow entry in place for retry"
        );
    }

    #[tokio::test]
    async fn finish_callback_state_mismatch_errors_and_preserves_the_flow() {
        let (store, _tmp) = test_store().await;
        let flow_key = insert_test_flow("wiz-cb-mismatch", "state-c", "verifier-c");
        let auth = AuthSpec {
            kind: AuthKind::Oauth,
            ..Default::default()
        };
        let callback = ryuzi_core::oauth_loopback::CallbackResult {
            code: Some("auth-code".into()),
            state: Some("not-state-c".into()),
        };
        let err =
            finish_plugin_oauth_callback(&store, "wiz-cb-mismatch", &auth, "state-c", callback)
                .await
                .unwrap_err();
        assert!(err.to_string().contains("state mismatch"), "{err}");
        assert!(
            plugin_oauth_flows()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .contains_key(&flow_key),
            "a state-mismatch callback must leave the flow entry in place — it wasn't this \
             install's response, so the pending install can still complete"
        );
    }

    #[tokio::test]
    async fn finish_callback_exchange_failure_reinserts_the_flow_for_retry() {
        let (store, _tmp) = test_store().await;
        let flow_key = insert_test_flow("wiz-cb-exchange", "state-d", "verifier-d");
        // No table row and no manifest endpoints/client id source at all —
        // exchange fails deterministically inside `exchange_plugin_oauth_code`
        // (via `plugin_oauth_prereq_error`) before any network call.
        let auth = AuthSpec {
            kind: AuthKind::Oauth,
            ..Default::default()
        };
        let callback = ryuzi_core::oauth_loopback::CallbackResult {
            code: Some("auth-code".into()),
            state: Some("state-d".into()),
        };
        let err =
            finish_plugin_oauth_callback(&store, "wiz-cb-exchange", &auth, "state-d", callback)
                .await
                .unwrap_err();
        assert!(err.to_string().contains("isn't ready"), "{err}");
        let flows = plugin_oauth_flows()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        let flow = flows
            .get(&flow_key)
            .expect("exchange failure must re-insert the flow so the wizard can retry");
        assert_eq!(
            flow.verifier, "verifier-d",
            "the re-inserted flow must be the same one that was removed, not a blank default"
        );
    }

    // ---------- SkillInstallBegin / TrustPromptDto mapping ----------

    #[tokio::test]
    async fn begin_skill_install_inner_prompts_for_arbitrary_source() {
        // Arbitrary sources can't be cloned offline; assert the mapping layer
        // instead by driving core's begin_install with a fake is impractical
        // here (install_skill_source resolves for_user). So assert the DTO
        // mapping from a constructed BeginInstall::NeedsConfirmation is correct.
        let prompt = ryuzi_core::skills_install::TrustPrompt {
            token: "t".into(),
            source_spec: "acme/p".into(),
            owner_repo: "acme/p".into(),
            resolved_commit: Some("c1".into()),
            skills: vec!["S".into()],
            hook_scripts: vec!["tool.before/g.sh".into()],
            total_bytes: 12,
        };
        let dto = SkillInstallBegin::from_needs_confirmation(prompt);
        assert!(!dto.completed);
        assert_eq!(
            dto.trust.unwrap().hook_scripts,
            vec!["tool.before/g.sh".to_string()]
        );
    }

    #[test]
    fn skill_install_begin_from_completed_carries_the_pack_and_no_trust_prompt() {
        let pack = InstalledSkillPack {
            id: "superpowers".into(),
            name: "Superpowers".into(),
            source: "https://github.com/obra/superpowers".into(),
            plugin_id: None,
            installed_at: "2026-07-11T00:00:00Z".into(),
            skills: vec![],
        };
        let dto = SkillInstallBegin::from_completed(pack.clone());
        assert!(dto.completed);
        assert!(dto.trust.is_none());
        assert_eq!(dto.plugin.unwrap(), pack);
    }

    #[test]
    fn update_outcome_dto_mirrors_every_variant_including_nested_trust_prompt() {
        assert!(matches!(
            UpdateOutcomeDto::from(UpdateOutcome::Updated),
            UpdateOutcomeDto::Updated
        ));
        assert!(matches!(
            UpdateOutcomeDto::from(UpdateOutcome::AlreadyCurrent),
            UpdateOutcomeDto::AlreadyCurrent
        ));
        assert!(matches!(
            UpdateOutcomeDto::from(UpdateOutcome::SkippedPinned),
            UpdateOutcomeDto::SkippedPinned
        ));
        assert!(matches!(
            UpdateOutcomeDto::from(UpdateOutcome::LocalEdits),
            UpdateOutcomeDto::LocalEdits
        ));
        match UpdateOutcomeDto::from(UpdateOutcome::Failed("boom".into())) {
            UpdateOutcomeDto::Failed(msg) => assert_eq!(msg, "boom"),
            other => panic!(
                "expected Failed, got a different variant: {}",
                other_kind(&other)
            ),
        }
        let prompt = TrustPrompt {
            token: "t2".into(),
            source_spec: "acme/q".into(),
            owner_repo: "acme/q".into(),
            resolved_commit: None,
            skills: vec![],
            hook_scripts: vec!["tool.after/h.sh".into()],
            total_bytes: 0,
        };
        match UpdateOutcomeDto::from(UpdateOutcome::NeedsReack(prompt)) {
            UpdateOutcomeDto::NeedsReack(dto) => {
                assert_eq!(dto.hook_scripts, vec!["tool.after/h.sh".to_string()]);
            }
            other => panic!(
                "expected NeedsReack, got a different variant: {}",
                other_kind(&other)
            ),
        }
    }

    fn other_kind(v: &UpdateOutcomeDto) -> &'static str {
        match v {
            UpdateOutcomeDto::Updated => "Updated",
            UpdateOutcomeDto::AlreadyCurrent => "AlreadyCurrent",
            UpdateOutcomeDto::SkippedPinned => "SkippedPinned",
            UpdateOutcomeDto::LocalEdits => "LocalEdits",
            UpdateOutcomeDto::Failed(_) => "Failed",
            UpdateOutcomeDto::NeedsReack(_) => "NeedsReack",
        }
    }

    #[test]
    fn doctor_finding_dto_mirrors_the_core_struct_field_for_field() {
        let core = doctor::DoctorFinding {
            plugin_id: "github".into(),
            severity: "warn".into(),
            kind: "attach-failed".into(),
            message: "github: authentication failed".into(),
            suggested_action: "Check github's configuration".into(),
        };
        let dto = DoctorFinding::from(core.clone());
        assert_eq!(dto.plugin_id, core.plugin_id);
        assert_eq!(dto.severity, core.severity);
        assert_eq!(dto.kind, core.kind);
        assert_eq!(dto.message, core.message);
        assert_eq!(dto.suggested_action, core.suggested_action);
    }

    #[tokio::test]
    async fn plugins_restart_required_reflects_the_control_plane_flag() {
        let cp = test_cp().await;
        assert!(!cp.plugins_restart_required());
        cp.mark_plugins_restart_required();
        assert!(cp.plugins_restart_required());
    }

    // ---------- plugins_restart_required wiring (Cockpit-side mutations) ----------

    #[tokio::test]
    async fn mark_restart_if_begin_completed_flips_flag_only_for_completed() {
        let cp = test_cp().await;
        let prompt = TrustPrompt {
            token: "t".into(),
            source_spec: "acme/p".into(),
            owner_repo: "acme/p".into(),
            resolved_commit: None,
            skills: vec![],
            hook_scripts: vec![],
            total_bytes: 0,
        };
        mark_restart_if_begin_completed(&cp, &BeginInstall::NeedsConfirmation(prompt));
        assert!(
            !cp.plugins_restart_required(),
            "a trust prompt alone hasn't touched disk yet"
        );

        let pack = InstalledSkillPack {
            id: "s".into(),
            name: "S".into(),
            source: "https://github.com/acme/s".into(),
            plugin_id: None,
            installed_at: "2026-07-11T00:00:00Z".into(),
            skills: vec![],
        };
        mark_restart_if_begin_completed(&cp, &BeginInstall::Completed(pack));
        assert!(cp.plugins_restart_required());
    }

    #[test]
    fn is_restart_required_update_true_only_for_updated() {
        assert!(is_restart_required_update(&UpdateOutcome::Updated));
        assert!(!is_restart_required_update(&UpdateOutcome::AlreadyCurrent));
        assert!(!is_restart_required_update(&UpdateOutcome::SkippedPinned));
        assert!(!is_restart_required_update(&UpdateOutcome::LocalEdits));
        assert!(!is_restart_required_update(&UpdateOutcome::Failed(
            "boom".into()
        )));
        let prompt = TrustPrompt {
            token: "t".into(),
            source_spec: "acme/p".into(),
            owner_repo: "acme/p".into(),
            resolved_commit: None,
            skills: vec![],
            hook_scripts: vec!["tool.before/g.sh".into()],
            total_bytes: 0,
        };
        assert!(!is_restart_required_update(&UpdateOutcome::NeedsReack(
            prompt
        )));
    }

    #[test]
    fn any_update_requires_restart_true_only_when_some_outcome_is_updated() {
        assert!(!any_update_requires_restart(&[
            ("a".to_string(), UpdateOutcome::AlreadyCurrent),
            ("b".to_string(), UpdateOutcome::SkippedPinned),
        ]));
        assert!(any_update_requires_restart(&[
            ("a".to_string(), UpdateOutcome::AlreadyCurrent),
            ("b".to_string(), UpdateOutcome::Updated),
        ]));
    }

    #[tokio::test]
    async fn plugins_restart_required_flips_on_uninstall_but_not_on_pin() {
        let cp = test_cp().await;
        assert!(!cp.plugins_restart_required());

        // A pin-only mutation must not require a restart — it doesn't change
        // what's on disk or loaded in-process.
        ryuzi_core::skills_install::set_pack_pin("nonexistent", true, None, cp.store())
            .await
            .unwrap();
        assert!(!cp.plugins_restart_required());

        // An uninstall does (using the hermetic integration-kind path
        // already exercised by
        // `uninstall_integration_clears_credential_and_disables` — "github",
        // not "discord": the `discord` gateway only registers under the
        // `discord` Cargo feature, which this crate's default test build
        // doesn't enable).
        uninstall_and_mark(&cp, "github").await.unwrap();
        assert!(cp.plugins_restart_required());
    }
}
