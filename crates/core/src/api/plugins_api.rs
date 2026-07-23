//! Plugins screen RPC family: every installed plugin's identity/capabilities
//! (`list_plugins`), a single plugin's full detail (`plugin_detail`),
//! enable/disable (`set_plugin_enabled` — delegates to
//! [`crate::plugins::toggle_enabled`], the same helper `ryuzi plugins
//! enable/disable` uses, so the two surfaces can never drift), a validated
//! settings write (`set_plugin_setting`), plugin OAuth sign-in, the install
//! wizard resolution (`begin_plugin_install`/`cancel_plugin_install`/
//! `set_plugin_oauth_client_id`), kind-symmetric `uninstall_plugin`, and a
//! provider's effective model list (`plugin_models`). Moved (per the Move
//! Recipe) from `apps/cockpit/src-tauri/src/plugins_cmd.rs`.
//!
//! DTOs here are deliberate thin mirrors of `ryuzi_plugin_sdk::PluginManifest`
//! (and [`crate::plugins::CorePlugin`]) rather than re-exports: the manifest
//! is the engine's contract for plugin authors, while these shapes are the
//! Cockpit UI's contract, free to add UI-only fields (like
//! `value_set`/`configured` booleans) without perturbing the engine type.
//!
//! Secrets are never returned: `PluginAuthInfo.configured` and
//! `PluginFieldInfo.value_set` are booleans derived from whether a row is
//! persisted (or an auth env var is set), never the value itself.
//!
//! Behavior change from the Tauri original: `begin_plugin_oauth` /
//! `begin_plugin_install` no longer take an `AppHandle` or open the system
//! browser directly — they broadcast [`CoreEvent::PluginOauthAuthorizeUrl`]
//! via `state.cp.emit(..)` and Cockpit opens the browser on receipt. The
//! loopback callback server (bind 8976), the browser open, and the local
//! flow-cancel handles stay Cockpit-local in `plugins_cmd.rs`; the daemon
//! owns discovery/DCR/token exchange and the PKCE flow map.

use super::{ok, params, ApiError};
use crate::api::types::*;
use crate::control::ControlPlane;
use crate::domain::CoreEvent;
use crate::plugins::oauth::{
    discover_oauth_server_metadata, generate_pkce_verifier, pkce_challenge_s256,
    register_oauth_client, OauthServerMetadata, PluginOauthToken,
};
use crate::plugins::providers;
use crate::plugins::{CorePlugin, PluginSource};
use crate::serve::ApiState;
use crate::settings::SettingsStore;
use crate::store::{PluginOauthClient, RemoteCatalogRow, Store};
use reqwest::Url;
use ryuzi_plugin_sdk::{
    AuthKind, AuthSpec, FieldKind, McpServerDef, McpTransportDef, SettingField,
};
use serde::Deserialize;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

pub(crate) const HANDLES: &[&str] = &[
    "list_plugins",
    "plugin_detail",
    "set_plugin_enabled",
    "set_plugin_setting",
    "begin_plugin_oauth",
    "complete_plugin_oauth",
    "disconnect_plugin_oauth",
    "plugin_models",
    "uninstall_plugin",
    "begin_plugin_install",
    "set_plugin_oauth_client_id",
    "cancel_plugin_install",
    "begin_skill_install",
    "confirm_skill_install",
    "update_plugin",
    "update_all_plugins",
    "set_plugin_pin",
    "plugin_doctor",
    "plugins_restart_required",
    // Component-plugin (WASM bundle) release management — Task 11a.
    "plugin_release_detail",
    "install_component_plugin",
    "rollback_component_plugin",
    "component_bootstrap_status",
    // Thin, profile-aware wrappers over the Phase-3 OAuth profile engine
    // (`plugins::capabilities::oauth`) — Task 11a.
    "plugin_profile_begin_pkce",
    "plugin_profile_disconnect",
    "plugin_profile_begin_device_flow",
    "plugin_profile_poll_device_flow",
];

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

#[derive(Deserialize)]
struct IdP {
    id: String,
}
#[derive(Deserialize)]
struct SetPluginEnabledP {
    id: String,
    enabled: bool,
}
#[derive(Deserialize)]
struct SetPluginSettingP {
    key: String,
    value: String,
}
#[derive(Deserialize)]
struct PluginIdP {
    plugin_id: String,
}
#[derive(Deserialize)]
struct CompletePluginOauthP {
    plugin_id: String,
    code: String,
    state_token: String,
}
#[derive(Deserialize)]
struct SetPluginOauthClientIdP {
    plugin_id: String,
    client_id: String,
}
#[derive(Deserialize)]
struct CancelPluginInstallP {
    plugin_id: String,
    state_token: Option<String>,
}
#[derive(Deserialize)]
struct SourceP {
    source: String,
}
#[derive(Deserialize)]
struct TokenP {
    token: String,
}
#[derive(Deserialize)]
struct UpdatePluginP {
    id: String,
    force: bool,
}
#[derive(Deserialize)]
struct SetPluginPinP {
    id: String,
    pinned: bool,
    reason: Option<String>,
}
#[derive(Deserialize)]
struct InstallComponentP {
    id: String,
    #[serde(default)]
    version: Option<String>,
}
#[derive(Deserialize)]
struct RollbackComponentP {
    id: String,
    /// The bad version to revoke + deactivate.
    from_version: String,
    /// The prior good version to re-point the active pointer at.
    to_version: String,
}
#[derive(Deserialize)]
struct ProfileBeginPkceP {
    plugin_id: String,
    profile_id: String,
    redirect_uri: String,
}
#[derive(Deserialize)]
struct ProfileIdP {
    plugin_id: String,
    profile_id: String,
}
#[derive(Deserialize)]
struct ProfileDeviceFlowP {
    plugin_id: String,
    profile_id: String,
    device_authorization_url: String,
}
#[derive(Deserialize)]
struct ProfilePollDeviceP {
    plugin_id: String,
    profile_id: String,
    token_url: String,
    device_code: String,
    expires_at: i64,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "list_plugins" => ok(assemble_list(cp).await?),
        "plugin_detail" => {
            let a: IdP = params(p)?;
            ok(assemble_detail(cp, &a.id).await?)
        }
        "set_plugin_enabled" => {
            let a: SetPluginEnabledP = params(p)?;
            set_plugin_enabled(cp, a.id, a.enabled).await?;
            ok(())
        }
        "set_plugin_setting" => {
            let a: SetPluginSettingP = params(p)?;
            set_plugin_setting(cp, a.key, a.value).await?;
            ok(())
        }
        "begin_plugin_oauth" => {
            let a: PluginIdP = params(p)?;
            ok(begin_plugin_oauth(cp, a.plugin_id).await?)
        }
        "complete_plugin_oauth" => {
            let a: CompletePluginOauthP = params(p)?;
            ok(complete_plugin_oauth(cp, a.plugin_id, a.code, a.state_token).await?)
        }
        "disconnect_plugin_oauth" => {
            let a: PluginIdP = params(p)?;
            ok(disconnect_plugin_oauth(cp, a.plugin_id).await?)
        }
        "plugin_models" => {
            let a: IdP = params(p)?;
            ok(providers::list_models(cp.store(), &a.id).await?)
        }
        "uninstall_plugin" => {
            let a: IdP = params(p)?;
            uninstall(cp, &a.id).await?;
            cp.mark_plugins_restart_required();
            ok(assemble_list(cp).await?)
        }
        "begin_plugin_install" => {
            let a: PluginIdP = params(p)?;
            ok(begin_plugin_install(cp, a.plugin_id).await?)
        }
        "set_plugin_oauth_client_id" => {
            let a: SetPluginOauthClientIdP = params(p)?;
            set_plugin_oauth_client_id(cp, a.plugin_id, a.client_id).await?;
            ok(())
        }
        "cancel_plugin_install" => {
            let a: CancelPluginInstallP = params(p)?;
            cancel_plugin_install(cp, a.plugin_id, a.state_token).await?;
            ok(())
        }
        "begin_skill_install" => {
            let a: SourceP = params(p)?;
            ok(begin_skill_install(cp, &a.source).await?)
        }
        "confirm_skill_install" => {
            let a: TokenP = params(p)?;
            ok(confirm_skill_install(cp, &a.token).await?)
        }
        "update_plugin" => {
            let a: UpdatePluginP = params(p)?;
            ok(update_plugin(cp, &a.id, a.force).await?)
        }
        "update_all_plugins" => ok(update_all_plugins(cp).await?),
        "set_plugin_pin" => {
            let a: SetPluginPinP = params(p)?;
            crate::skills_install::set_pack_pin(&a.id, a.pinned, a.reason.as_deref(), cp.store())
                .await?;
            ok(())
        }
        "plugin_doctor" => {
            let findings = crate::plugins::doctor::plugin_doctor(cp).await?;
            ok(findings
                .into_iter()
                .map(DoctorFinding::from)
                .collect::<Vec<_>>())
        }
        "plugins_restart_required" => ok(cp.plugins_restart_required()),
        "plugin_release_detail" => {
            let a: IdP = params(p)?;
            ok(plugin_release_detail(cp, &a.id).await?)
        }
        "install_component_plugin" => {
            let a: InstallComponentP = params(p)?;
            ok(install_component_plugin(cp, &a.id, a.version.as_deref()).await?)
        }
        "rollback_component_plugin" => {
            let a: RollbackComponentP = params(p)?;
            ok(rollback_component_plugin(cp, &a.id, &a.from_version, &a.to_version).await?)
        }
        "component_bootstrap_status" => ok(component_bootstrap_status(cp).await?),
        "plugin_profile_begin_pkce" => {
            let a: ProfileBeginPkceP = params(p)?;
            ok(plugin_profile_begin_pkce(cp, &a.plugin_id, &a.profile_id, &a.redirect_uri).await?)
        }
        "plugin_profile_disconnect" => {
            let a: ProfileIdP = params(p)?;
            plugin_profile_disconnect(cp, &a.plugin_id, &a.profile_id).await?;
            ok(())
        }
        "plugin_profile_begin_device_flow" => {
            let a: ProfileDeviceFlowP = params(p)?;
            ok(plugin_profile_begin_device_flow(
                cp,
                &a.plugin_id,
                &a.profile_id,
                &a.device_authorization_url,
            )
            .await?)
        }
        "plugin_profile_poll_device_flow" => {
            let a: ProfilePollDeviceP = params(p)?;
            ok(plugin_profile_poll_device_flow(
                cp,
                &a.plugin_id,
                &a.profile_id,
                &a.token_url,
                &a.device_code,
                a.expires_at,
            )
            .await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

/// Phase 1 of the two-phase tiered trust gate (see
/// [`crate::skills_install::begin_install`]): curated sources install
/// immediately (`completed: true`); arbitrary sources stop at a trust prompt
/// the wizard must show before `confirm_skill_install` can proceed. Marks the
/// daemon dirty (`plugins_restart_required`) only when the install actually
/// completed — a `NeedsConfirmation` trust prompt hasn't touched disk yet.
async fn begin_skill_install(cp: &ControlPlane, source: &str) -> anyhow::Result<SkillInstallBegin> {
    let result = crate::skills_install::begin_install(source, cp.store()).await?;
    if matches!(result, crate::skills_install::BeginInstall::Completed(_)) {
        cp.mark_plugins_restart_required();
    }
    Ok(SkillInstallBegin::from(result))
}

/// Phase 2: complete a staged install (or update) after the user has
/// acknowledged its trust prompt. The token is single-use. Always marks
/// `plugins_restart_required`: reaching this point always means an install (or
/// reack-triggered update) just completed.
async fn confirm_skill_install(
    cp: &ControlPlane,
    token: &str,
) -> anyhow::Result<crate::skills_install::InstalledSkillPack> {
    let pack = crate::skills_install::confirm_install(token, cp.store()).await?;
    cp.mark_plugins_restart_required();
    Ok(pack)
}

/// Update one installed pack. `force` overrides the local-edits guard but
/// never the pinned guard or the hook-script re-ack gate. Marks a restart only
/// on an actual `Updated` outcome — the other outcomes are no-ops on disk.
async fn update_plugin(
    cp: &ControlPlane,
    id: &str,
    force: bool,
) -> anyhow::Result<UpdateOutcomeDto> {
    let outcome = crate::skills_install::update_installed_pack(id, force, cp.store()).await?;
    if matches!(outcome, crate::skills_install::UpdateOutcome::Updated) {
        cp.mark_plugins_restart_required();
    }
    Ok(UpdateOutcomeDto::from(outcome))
}

/// Update every installed pack (skipping pinned ones); never fails as a whole
/// — a single pack's error surfaces as that pack's `Failed` entry. Marks a
/// restart if at least one pack actually reinstalled.
async fn update_all_plugins(cp: &ControlPlane) -> anyhow::Result<Vec<UpdateOutcomeEntry>> {
    let results = crate::skills_install::update_all_packs(cp.store()).await?;
    if results
        .iter()
        .any(|(_, o)| matches!(o, crate::skills_install::UpdateOutcome::Updated))
    {
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

fn source_label(source: &PluginSource) -> &'static str {
    match source {
        PluginSource::Builtin => "builtin",
        PluginSource::Catalog | PluginSource::RemoteCatalog => "catalog",
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
    if plugin.harness.is_some() {
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

/// `PluginInfo.catalogSource` for a plugin's binding source: `embedded` for
/// the compiled-in catalog, `remote` for a signed-feed entry that won a
/// version-gated merge (`crate::plugins::catalog::merged_catalog_plugins`),
/// `None` for builtins and skill packs — neither catalog ever produces them.
fn catalog_source_label(source: &PluginSource) -> Option<String> {
    match source {
        PluginSource::Catalog => Some("embedded".to_string()),
        PluginSource::RemoteCatalog => Some("remote".to_string()),
        PluginSource::Builtin | PluginSource::SkillPack(_) => None,
    }
}

/// Family head id for a provider plugin (`anthropic-oauth` → `anthropic`).
fn provider_family(id: &str) -> String {
    crate::llm_router::registry::descriptor(id)
        .map(|d| d.family.to_string())
        .unwrap_or_else(|| id.to_string())
}

/// Pure kind → installed decision. Inputs are pre-computed by the caller.
fn installed_flag(
    kind: &str,
    enabled: bool,
    configured: bool,
    provider_installed: bool,
    gateway_settings_complete: bool,
    skill_pack_installed: bool,
) -> bool {
    match kind {
        "provider" => provider_installed,
        "gateway" => gateway_settings_complete,
        "skill-pack" => skill_pack_installed,
        _ => configured || enabled,
    }
}

/// Ledger-derived `PluginInfo` fields (`pinned`, `sourceSpec`,
/// `resolvedCommit`, `installedAt`, `updatedAt`, `trustTier`) drawn from an
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

    fn from_record(rec: &crate::store::PluginInstallRecord) -> Self {
        Self {
            pinned: rec.pinned,
            source_spec: Some(rec.source_spec.clone()),
            resolved_commit: rec.resolved_commit.clone(),
            installed_at: Some(rec.installed_at),
            updated_at: Some(rec.updated_at),
            trust_tier: Some(rec.trust_tier.clone()),
        }
    }

    fn from_option(rec: Option<&crate::store::PluginInstallRecord>) -> Self {
        rec.map(Self::from_record).unwrap_or_else(Self::absent)
    }
}

/// Enrichment inputs `plugin_info` needs beyond the plugin itself: the
/// install ledger row, the cached remote-catalog row, and whether this
/// plugin currently owns its manifest-claimed `slot` (Feature C2). Bundled
/// into one struct so `plugin_info` doesn't creep past clippy's
/// too-many-arguments lint as fields get added over time.
struct PluginInfoContext<'a> {
    install: Option<&'a crate::store::PluginInstallRecord>,
    remote: Option<&'a RemoteCatalogRow>,
    owns_slot: bool,
}

fn plugin_info(
    plugin: &CorePlugin,
    enabled: bool,
    configured: bool,
    kind: &str,
    installed: bool,
    ctx: PluginInfoContext<'_>,
) -> PluginInfo {
    let m = &plugin.manifest;
    let ledger = InstallLedgerFields::from_option(ctx.install);
    let remote = ctx.remote;
    let owns_slot = ctx.owns_slot;
    PluginInfo {
        id: m.id.clone(),
        name: m.name.clone(),
        description: m.description.clone(),
        icon: m.icon.clone(),
        categories: m.categories.clone(),
        slot: m.slot.clone(),
        owns_slot,
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
        catalog_source: catalog_source_label(&plugin.source),
        catalog_version: remote.map(|r| r.version.clone()),
        blocked_reason: remote.and_then(|r| r.blocked_reason.clone()),
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

/// `ryuzi_plugin_sdk::FieldKind` -> the camelCase-friendly label
/// `PluginFieldInfo.kind` carries across the Tauri IPC boundary.
fn field_kind_label(kind: FieldKind) -> &'static str {
    match kind {
        FieldKind::String => "string",
        FieldKind::Int => "int",
        FieldKind::Bool => "bool",
    }
}

fn plugin_oauth_flow_key(plugin_id: &str, state_token: &str) -> String {
    format!("{plugin_id}:{state_token}")
}

/// The install wizard's loopback callback server port. Registered redirect
/// URIs use it, so it can never change without re-registering every DCR
/// client. The daemon builds the redirect_uri string; Cockpit binds the same
/// port with `oauth_loopback::bind_fixed`.
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

/// Drop pending daemon-side flow state for `plugin_id` — all of its flows
/// when `state_token` is `None`, else just that one. The loopback callback
/// server this feeds is Cockpit-local (`plugins_cmd.rs`); only the daemon's
/// PKCE/verifier map lives here.
fn drop_pending_plugin_flows(plugin_id: &str, state_token: Option<&str>) {
    let prefix = format!("{plugin_id}:");
    let mut flows = plugin_oauth_flows()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    match state_token {
        Some(token) => {
            flows.remove(&plugin_oauth_flow_key(plugin_id, token));
        }
        None => {
            let keys: Vec<String> = flows
                .keys()
                .filter(|k| k.starts_with(&prefix))
                .cloned()
                .collect();
            for key in keys {
                flows.remove(&key);
            }
        }
    }
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

/// Build the authorize URL from already-resolved endpoints/client id — table
/// values take precedence over manifest fields (see [`resolve_plugin_oauth`]).
/// `begin_plugin_install` calls this directly with its post-DCR resolution;
/// `begin_plugin_oauth` goes through the async wrapper above.
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
/// dynamic-registration flag) → client id / DCR → authorize URL + flow state.
/// Kept free of the browser/loopback steps so tests can drive it against a
/// mock vendor; the Cockpit-local `begin_plugin_install` proxy wraps it with
/// the callback server (step 7). The daemon RPC below emits the authorize URL
/// (step 8) as a `CoreEvent`.
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

    // 4. Endpoint resolution: discover when either endpoint COLUMN is missing
    // — regardless of the dynamic-registration flag (Slack needs endpoints
    // too). Manifest endpoints can still rescue a failure.
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
                    // Persist endpoints even when registration is impossible —
                    // the manual client-id path needs an authorize URL.
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
        // Discovery failed and neither cache nor manifest supplies endpoints —
        // nothing else is possible; the wizard shows dcr_error with Retry.
        return Ok(result);
    }

    // 5. Client id: any existing id (row → client_id_setting) permanently
    // suppresses DCR. DCR runs only when the manifest opts in AND this call's
    // discovery document exposed a registration_endpoint.
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
    drop_pending_plugin_flows(plugin_id, None);
    let verifier = generate_pkce_verifier();
    let state_token = crate::paths::new_id();
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

/// `PluginAuthInfo.configured` for the list payload without building the whole
/// auth DTO: oauth → a token is stored and reconnect isn't required; otherwise
/// the `auth.setting`-row / `auth.env` check. No `[auth]` → false.
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

/// Whether a settings field's value is set: a persisted, non-empty row. Pure —
/// callers resolve the persisted row first (see `build_settings_info`).
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
            kind: field_kind_label(f.kind).to_string(),
            options: f.options.clone(),
            default: f.default.clone(),
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

/// Raw manifest string, no `${auth}` substitution — command for stdio, url for
/// http.
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
        .map(|seconds| crate::paths::now_ms() + seconds.saturating_mul(1000));

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

struct InstalledCtx {
    installed_skills: Vec<crate::skills_install::InstalledSkillInfo>,
    installed_providers: Vec<String>,
}

async fn installed_ctx(store: &Store) -> anyhow::Result<InstalledCtx> {
    Ok(InstalledCtx {
        installed_skills: crate::skills_install::list_installed_skills().unwrap_or_default(),
        installed_providers: crate::llm_router::installed::list_installed_providers(store).await?,
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
    // Provider installed-ness is authoritative on the persisted installed set
    // ALONE — never on whether a connection exists. The Models list filters on
    // the same set, so both surfaces agree in lockstep. Existing-connection
    // families are unioned into the set at boot by
    // `ensure_default_installed_providers`, and connections are only ever added
    // to already-installed providers, so a real connection always has its
    // family in the set.
    let provider_installed = kind == "provider"
        && crate::llm_router::installed::is_installed(
            &ctx.installed_providers,
            &provider_family(id),
        );
    let gateway_settings_complete = if kind == "gateway" {
        // A gateway with no manifest settings has nothing to configure, so its
        // installed-ness is just whether it's enabled — otherwise it could
        // never leave Browse. A gateway that declares required settings takes
        // the all-present path below.
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
        provider_installed,
        gateway_settings_complete,
        skill_pack_installed,
    ))
}

/// Fetch every `plugin_installs` ledger row ONCE and index it by plugin id so
/// list assembly stays O(1) round-trips regardless of the plugin count (never
/// a per-plugin `get_plugin_install` inside the loop below).
async fn install_ledger_index(
    store: &Store,
) -> anyhow::Result<HashMap<String, crate::store::PluginInstallRecord>> {
    Ok(store
        .list_plugin_installs()
        .await?
        .into_iter()
        .map(|r| (r.plugin_id.clone(), r))
        .collect())
}

/// Fetch every cached `plugin_catalog_cache` row ONCE and index it by plugin
/// id — mirrors [`install_ledger_index`]'s O(1)-round-trip shape, so list
/// assembly never issues a per-plugin remote-catalog query.
async fn remote_catalog_index(store: &Store) -> anyhow::Result<HashMap<String, RemoteCatalogRow>> {
    Ok(store
        .list_remote_catalog()
        .await?
        .into_iter()
        .map(|r| (r.id.clone(), r))
        .collect())
}

async fn assemble_list(cp: &ControlPlane) -> anyhow::Result<Vec<PluginInfo>> {
    let settings = SettingsStore::new(cp.store().clone());
    let ctx = installed_ctx(cp.store()).await?;
    let installs = install_ledger_index(cp.store()).await?;
    let remote = remote_catalog_index(cp.store()).await?;
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
        let remote_row = remote.get(&plugin.manifest.id);
        let owns_slot = plugin
            .manifest
            .slot
            .as_deref()
            .is_some_and(|s| cp.plugins().slot_owner(s) == Some(plugin.manifest.id.as_str()));
        out.push(plugin_info(
            &plugin,
            enabled,
            configured,
            kind,
            installed,
            PluginInfoContext {
                install: record,
                remote: remote_row,
                owns_slot,
            },
        ));
    }
    for pack in crate::skills_install::curated_skill_packs() {
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
            // A synthesized curated pack has no manifest to declare a slot.
            slot: None,
            owns_slot: false,
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
            // A synthesized curated pack is never sourced from either
            // catalog (it resolves via git clone, not a manifest feed).
            catalog_source: None,
            catalog_version: None,
            blocked_reason: None,
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
    let remote_row = cp
        .store()
        .list_remote_catalog()
        .await?
        .into_iter()
        .find(|r| r.id == id);

    let owns_slot = m
        .slot
        .as_deref()
        .is_some_and(|s| cp.plugins().slot_owner(s) == Some(id));

    Ok(PluginDetail {
        info: plugin_info(
            &plugin,
            enabled,
            configured,
            kind,
            installed,
            PluginInfoContext {
                install: record.as_ref(),
                remote: remote_row.as_ref(),
                owns_slot,
            },
        ),
        auth,
        settings: settings_info,
        mcp,
        models,
        homepage: m.homepage.clone(),
        publisher: m.publisher.clone(),
    })
}

/// Same semantics as `ryuzi plugins enable/disable` — delegates to the shared
/// core helper so the two surfaces never drift.
async fn set_plugin_enabled(cp: &ControlPlane, id: String, enabled: bool) -> Result<(), ApiError> {
    let settings = SettingsStore::new(cp.store().clone());
    crate::plugins::toggle_enabled(cp.plugins(), &settings, &id, enabled).await?;
    Ok(())
}

/// Validated write through `SettingsStore::set` — rejects unknown keys and
/// type-mismatched values the same way `ryuzi config set` does. Never returns
/// a value, so no secret can leak back through this command.
async fn set_plugin_setting(cp: &ControlPlane, key: String, value: String) -> Result<(), ApiError> {
    let settings = SettingsStore::new(cp.store().clone());
    settings.set(&key, &value).await?;
    Ok(())
}

/// Kind-symmetric uninstall: after this the entry's `installed` flips false and
/// it reappears in Browse.
async fn uninstall(cp: &ControlPlane, id: &str) -> anyhow::Result<()> {
    let settings = SettingsStore::new(cp.store().clone());
    let Some(plugin) = cp.plugins().get(id) else {
        // Synthesized curated pack or a pack installed without a manifest —
        // resolve through the skills installer.
        let installed = crate::skills_install::list_installed_skills()?;
        let Some(pack) = installed
            .iter()
            .find(|s| s.id == id || s.source == id || s.plugin_id.as_deref() == Some(id))
        else {
            anyhow::bail!("unknown plugin: {id}");
        };
        // Recorded variant: also drop the pack's `plugin_installs` +
        // `plugin_attach_status` rows, so an uninstalled pack doesn't leave a
        // ghost ledger row (which would make every future `update_all_packs`
        // report `Failed("unknown installed skill: <id>")` for it and bleed
        // stale trust/pin/attach metadata into the reappeared Browse card).
        return crate::skills_install::remove_installed_skill_recorded(&pack.id, cp.store()).await;
    };
    match derive_kind(&plugin) {
        Some("provider") => {
            let family = provider_family(id);
            for row in crate::llm_router::connections::list_connections(cp.store()).await? {
                if provider_family(&row.provider) == family {
                    crate::llm_router::connections::remove_connection(cp.store(), &row.id).await?;
                }
            }
            Ok(())
        }
        Some("gateway") => {
            for field in &plugin.manifest.settings {
                cp.store().delete_setting_raw(&field.key).await?;
            }
            crate::plugins::toggle_enabled(cp.plugins(), &settings, id, false).await
        }
        Some("skill-pack") => {
            let installed = crate::skills_install::list_installed_skills()?;
            let Some(pack) = installed
                .iter()
                .find(|s| s.plugin_id.as_deref() == Some(id) || s.id == id)
            else {
                anyhow::bail!("skill pack not installed: {id}");
            };
            // Recorded variant — drops the ledger row too (see the
            // not-in-host fallback above for why a ghost row is harmful).
            crate::skills_install::remove_installed_skill_recorded(&pack.id, cp.store()).await
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
                crate::plugins::toggle_enabled(cp.plugins(), &settings, id, false).await?;
            }
            Ok(())
        }
    }
}

async fn begin_plugin_oauth(
    cp: &ControlPlane,
    plugin_id: String,
) -> Result<PluginOauthBeginResult, ApiError> {
    let auth = plugin_oauth_auth(cp, &plugin_id)?;
    let verifier = generate_pkce_verifier();
    let state_token = crate::paths::new_id();
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
    cp.emit(CoreEvent::PluginOauthAuthorizeUrl {
        plugin_id,
        authorize_url: begin.authorize_url.clone(),
    });
    Ok(begin)
}

async fn complete_plugin_oauth(
    cp: &ControlPlane,
    plugin_id: String,
    code: String,
    state_token: String,
) -> Result<PluginAuthInfo, ApiError> {
    let auth = plugin_oauth_auth(cp, &plugin_id)?;
    let flow_key = plugin_oauth_flow_key(&plugin_id, &state_token);
    let flow = plugin_oauth_flows()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .remove(&flow_key)
        .ok_or_else(|| ApiError::bad_request("plugin sign-in flow not found — start again"))?;
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

async fn disconnect_plugin_oauth(
    cp: &ControlPlane,
    plugin_id: String,
) -> Result<PluginAuthInfo, ApiError> {
    let auth = plugin_oauth_auth(cp, &plugin_id)?;
    cp.store().delete_plugin_oauth_token(&plugin_id).await?;
    Ok(build_auth_info(cp.store(), &plugin_id, &auth).await?)
}

/// The install wizard's entry point (spec 8-step resolution order). Steps 1-6
/// live in `resolve_plugin_install`; the daemon adds step 8 here (emit
/// `CoreEvent::PluginOauthAuthorizeUrl`, which the Cockpit SSE bridge maps to
/// a browser open). Step 7 (bind 8976 + background callback/exchange task)
/// stays Cockpit-local in the `begin_plugin_install` proxy, so
/// `callback_mode` is left `"manual"` here — Cockpit flips it to `"auto"`
/// after a successful local bind.
async fn begin_plugin_install(
    cp: &ControlPlane,
    plugin_id: String,
) -> Result<PluginInstallBeginResult, ApiError> {
    let plugin = cp
        .plugins()
        .get(&plugin_id)
        .ok_or_else(|| ApiError::not_found(format!("unknown plugin: {plugin_id}")))?;
    let auth = plugin.manifest.auth.clone();
    let http = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(15))
        .build()
        .map_err(|e| ApiError {
            status: 500,
            message: e.to_string(),
        })?;
    let result = resolve_plugin_install(cp.store(), &http, &plugin_id, auth.as_ref()).await?;
    if let Some(begin) = result.oauth_begin.clone() {
        cp.emit(CoreEvent::PluginOauthAuthorizeUrl {
            plugin_id: plugin_id.clone(),
            authorize_url: begin.authorize_url.clone(),
        });
    }
    Ok(result)
}

/// Persist a manually-entered client id. External-OAuth plugins store it under
/// the declared `auth.setting` via the validated SettingsStore path
/// (`validate_setting`/`register_plugin_fields` only accept manifest-declared
/// keys); everyone else upserts `plugin_oauth_clients.client_id` — deliberately
/// NOT a `plugin.*` setting, since none of these manifests declare one.
async fn set_plugin_oauth_client_id(
    cp: &ControlPlane,
    plugin_id: String,
    client_id: String,
) -> Result<(), ApiError> {
    let auth = plugin_oauth_auth(cp, &plugin_id)?;
    let client_id = client_id.trim();
    if client_id.is_empty() {
        return Err(ApiError::bad_request("client id must not be empty"));
    }
    if is_external_oauth(&auth) {
        let Some(key) = auth.setting.as_deref() else {
            return Err(ApiError::bad_request(format!(
                "{plugin_id} declares no auth.setting to hold a client id"
            )));
        };
        let settings = SettingsStore::new(cp.store().clone());
        settings.set(key, client_id).await?;
        return Ok(());
    }
    cp.store()
        .upsert_plugin_oauth_client(&PluginOauthClient {
            plugin_id: plugin_id.clone(),
            authorize_url: None,
            token_url: None,
            client_id: Some(client_id.to_string()),
        })
        .await?;
    Ok(())
}

/// Cancel the pending OAuth flow for this plugin, if any (daemon half): drops
/// the flow-map entry. `state_token` narrows to a specific flow when known;
/// `None` cancels whatever is pending for the id. Shutting down the local
/// loopback callback listener is the Cockpit half (`plugins_cmd.rs`).
async fn cancel_plugin_install(
    cp: &ControlPlane,
    plugin_id: String,
    state_token: Option<String>,
) -> Result<(), ApiError> {
    if cp.plugins().get(&plugin_id).is_none() {
        return Err(ApiError::not_found(format!("unknown plugin: {plugin_id}")));
    }
    drop_pending_plugin_flows(&plugin_id, state_token.as_deref());
    Ok(())
}

// ===========================================================================
// Component-plugin (WASM bundle) release management — Task 11a.
// ===========================================================================

/// The release ledger for a component plugin: every recorded release (oldest
/// first) plus the active version. Read-only; the template is `plugin_detail`.
///
/// Task 12 addition: when a version is active, also resolves that version's
/// on-disk bundle manifest (publisher/lifecycle/domains/oauth) for the
/// permission-confirmation summary — see [`ComponentManifestInfo`]'s doc for
/// why this is read-only-disk, not a new network fetch, and why it is `None`
/// for a never-installed plugin. Best-effort: any I/O error (most commonly,
/// the bundle root not existing yet) degrades to `None` rather than failing
/// the whole RPC, since this is read-only display data.
async fn plugin_release_detail(
    cp: &ControlPlane,
    plugin_id: &str,
) -> anyhow::Result<ComponentReleaseDetail> {
    let releases = cp.store().list_component_releases(plugin_id).await?;
    let active_version = cp
        .store()
        .active_component_release(plugin_id)
        .await?
        .map(|r| r.version);
    let active_manifest = if active_version.is_some() {
        let root = crate::plugins::bundle::installed_bundle_root();
        crate::plugins::bundle::load_active_bundles(&root, cp.store())
            .await
            .ok()
            .and_then(|bundles| {
                bundles
                    .into_iter()
                    .find(|b| b.manifest.id == plugin_id)
                    .map(|b| ComponentManifestInfo::from(b.manifest))
            })
    } else {
        None
    };
    Ok(ComponentReleaseDetail {
        plugin_id: plugin_id.to_string(),
        releases: releases
            .into_iter()
            .map(ComponentReleaseInfo::from)
            .collect(),
        active_version,
        active_manifest,
    })
}

/// Install (or update to) a component plugin's signed release via the Task 11a
/// pipeline (resolve+download+stage+verify_bundle+install+activate), then mark
/// the host restart-required so the newly activated bundle is picked up.
/// Returns the release ledger after the install. Fail-closed: with no
/// first-party signing key yet (the placeholder), this refuses before any
/// network I/O rather than staging an unverifiable bundle.
async fn install_component_plugin(
    cp: &ControlPlane,
    plugin_id: &str,
    version: Option<&str>,
) -> anyhow::Result<ComponentReleaseDetail> {
    let store = cp.store();
    let trusted_keys = crate::plugins::first_party_key::first_party_trusted_keys();
    if trusted_keys.is_empty() {
        anyhow::bail!(
            "component plugin installs are disabled until the first-party signing key is configured"
        );
    }
    let base_url = store
        .get_setting_raw("component_release_base_url")
        .await?
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| {
            crate::plugins::remote_catalog::DEFAULT_COMPONENT_RELEASE_BASE_URL.to_string()
        });
    let http = crate::plugins::remote_catalog::ReqwestCatalogHttp::new();
    let installer = crate::plugins::bundle::ComponentBundleInstaller::new(
        crate::plugins::bundle::installed_bundle_root(),
        store.as_ref().clone(),
    );
    crate::plugins::remote_catalog::install_component_release(
        &http,
        &installer,
        &trusted_keys,
        &base_url,
        plugin_id,
        version,
    )
    .await?;
    cp.mark_plugins_restart_required();
    plugin_release_detail(cp, plugin_id).await
}

/// Roll a component plugin off a bad release: re-point the active release to the
/// prior-good `to_version`, revoke + deactivate `from_version`, and mark the
/// host restart-required so the rolled-back bundle is loaded fresh on the next
/// session/boot (the same reload signal `uninstall_plugin` uses).
///
/// ORDER MATTERS: `set_active_component_release` runs first. It validates — in
/// one transaction, before mutating anything — that `to_version` exists and is
/// not revoked, so a missing/revoked target is a clean no-op that leaves
/// `from_version` still active. Only once the good version is active do we
/// revoke the bad one, so a failed reactivation can NEVER strand the plugin with
/// no active release (the non-atomic revoke-first ordering could).
async fn rollback_component_plugin(
    cp: &ControlPlane,
    plugin_id: &str,
    from_version: &str,
    to_version: &str,
) -> anyhow::Result<ComponentReleaseDetail> {
    if from_version == to_version {
        anyhow::bail!(
            "cannot roll back {plugin_id} to the same version being revoked ({from_version})"
        );
    }
    let store = cp.store();
    store
        .set_active_component_release(plugin_id, to_version)
        .await?;
    store
        .mark_component_release_revoked(
            plugin_id,
            from_version,
            &format!("rolled back to {to_version}"),
        )
        .await?;
    cp.mark_plugins_restart_required();
    plugin_release_detail(cp, plugin_id).await
}

/// The first-party component bootstrap's retryable status: pending when the
/// last bootstrap attempt landed nothing AND bootstrap has not since completed,
/// so Cockpit (Task 12) can surface a retry banner.
async fn component_bootstrap_status(cp: &ControlPlane) -> anyhow::Result<ComponentBootstrapStatus> {
    let store = cp.store();
    let message = store
        .get_setting_raw(crate::plugins::remote_catalog::FIRST_PARTY_BOOTSTRAP_RETRY)
        .await?
        .filter(|m| !m.is_empty());
    let completed = store
        .get_setting_raw(crate::plugins::remote_catalog::FIRST_PARTY_BOOTSTRAP_MARKER)
        .await?
        .is_some();
    let pending = message.is_some() && !completed;
    Ok(ComponentBootstrapStatus {
        message: pending.then(|| message.clone().unwrap_or_default()),
        pending,
    })
}

// ---------------------------------------------------------------------------
// Thin, profile-aware wrappers over the Phase-3 OAuth profile engine
// (`plugins::capabilities::oauth::ProfileOauth`). No new OAuth engine logic —
// each handler just builds the plugin's capability context from its installed
// bundle (so the network allowlist and declared profile set come from the
// signed manifest, never the caller) and dispatches one method. Deliberately
// minimal: mimo/opencode don't use OAuth (that lands with Task 13/GitHub), and
// `authorized_request` is a component-runtime-facing HTTP proxy, not a Cockpit
// surface, so it is intentionally NOT exposed here.
// ---------------------------------------------------------------------------

/// Map a capability-adapter `OauthErr` to an `ApiError` status.
fn oauth_err(err: crate::plugins::capabilities::oauth::OauthErr) -> ApiError {
    use crate::plugins::capabilities::oauth::OauthErr;
    match err {
        OauthErr::InvalidRequest(message) => ApiError::bad_request(message),
        OauthErr::Denied => ApiError {
            status: 403,
            message: "oauth profile denied".to_string(),
        },
        OauthErr::Expired => ApiError::conflict("oauth token expired"),
        OauthErr::Failed(message) => ApiError {
            status: 502,
            message,
        },
    }
}

/// Load the active installed bundle for `plugin_id` and build its capability
/// context (+ return the manifest). The context's network allowlist and OAuth
/// profile ids come from the signed bundle manifest, so a component can never
/// widen its own permissions through these RPCs. Telemetry is a no-op here (the
/// wrapped `ProfileOauth` methods don't emit).
async fn profile_capability_context(
    cp: &ControlPlane,
    plugin_id: &str,
) -> Result<
    (
        crate::plugins::capabilities::PluginCapabilityContext,
        ryuzi_plugin_sdk::PluginBundleManifest,
    ),
    ApiError,
> {
    let root = crate::plugins::bundle::installed_bundle_root();
    let bundles = crate::plugins::bundle::load_active_bundles(&root, cp.store())
        .await
        .map_err(ApiError::from)?;
    let bundle = bundles
        .into_iter()
        .find(|b| b.manifest.id == plugin_id)
        .ok_or_else(|| {
            ApiError::not_found(format!("no active component bundle for {plugin_id}"))
        })?;
    let manifest = bundle.manifest.clone();
    let ctx = crate::plugins::capabilities::PluginCapabilityContext {
        plugin_id: manifest.id.clone(),
        version: manifest.version.clone(),
        settings: SettingsStore::new(cp.store().clone()),
        store: cp.store().clone(),
        telemetry: std::sync::Arc::new(crate::telemetry::NoopTelemetry),
        network_allowlist: manifest
            .permissions
            .network
            .iter()
            .map(|entry| entry.0.clone())
            .collect(),
        oauth_profile_ids: manifest.oauth.iter().map(|p| p.id.clone()).collect(),
        provider_ids: manifest.resolved_provider_ids(),
    };
    Ok((ctx, manifest))
}

fn find_oauth_profile(
    manifest: &ryuzi_plugin_sdk::PluginBundleManifest,
    profile_id: &str,
) -> Result<ryuzi_plugin_sdk::OAuthProfile, ApiError> {
    manifest
        .oauth
        .iter()
        .find(|p| p.id == profile_id)
        .cloned()
        .ok_or_else(|| {
            ApiError::not_found(format!(
                "plugin does not declare oauth profile {profile_id:?}"
            ))
        })
}

fn device_poll_outcome_label(
    outcome: crate::plugins::capabilities::oauth::DevicePollOutcome,
) -> &'static str {
    use crate::plugins::capabilities::oauth::DevicePollOutcome;
    match outcome {
        DevicePollOutcome::Pending => "pending",
        DevicePollOutcome::SlowDown => "slow-down",
        DevicePollOutcome::Ready => "ready",
        DevicePollOutcome::Expired => "expired",
        DevicePollOutcome::Denied => "denied",
    }
}

async fn plugin_profile_begin_pkce(
    cp: &ControlPlane,
    plugin_id: &str,
    profile_id: &str,
    redirect_uri: &str,
) -> Result<PluginProfilePkceStart, ApiError> {
    let (ctx, manifest) = profile_capability_context(cp, plugin_id).await?;
    let profile = find_oauth_profile(&manifest, profile_id)?;
    let start = crate::plugins::capabilities::oauth::ProfileOauth::new(&ctx)
        .begin_pkce(&profile, redirect_uri)
        .await
        .map_err(oauth_err)?;
    Ok(start.into())
}

async fn plugin_profile_disconnect(
    cp: &ControlPlane,
    plugin_id: &str,
    profile_id: &str,
) -> Result<(), ApiError> {
    let (ctx, _manifest) = profile_capability_context(cp, plugin_id).await?;
    crate::plugins::capabilities::oauth::ProfileOauth::new(&ctx)
        .disconnect_profile(profile_id)
        .await
        .map_err(oauth_err)
}

async fn plugin_profile_begin_device_flow(
    cp: &ControlPlane,
    plugin_id: &str,
    profile_id: &str,
    device_authorization_url: &str,
) -> Result<PluginProfileDeviceFlowStart, ApiError> {
    let (ctx, manifest) = profile_capability_context(cp, plugin_id).await?;
    let profile = find_oauth_profile(&manifest, profile_id)?;
    let start = crate::plugins::capabilities::oauth::ProfileOauth::new(&ctx)
        .begin_device_flow(&profile, device_authorization_url)
        .await
        .map_err(oauth_err)?;
    Ok(start.into())
}

async fn plugin_profile_poll_device_flow(
    cp: &ControlPlane,
    plugin_id: &str,
    profile_id: &str,
    token_url: &str,
    device_code: &str,
    expires_at: i64,
) -> Result<String, ApiError> {
    let (ctx, manifest) = profile_capability_context(cp, plugin_id).await?;
    let profile = find_oauth_profile(&manifest, profile_id)?;
    let outcome = crate::plugins::capabilities::oauth::ProfileOauth::new(&ctx)
        .poll_device_flow(&profile, token_url, device_code, expires_at)
        .await
        .map_err(oauth_err)?;
    Ok(device_poll_outcome_label(outcome).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{dispatch, tests_support::state};
    use crate::connector::{Connector, ConnectorCtx};
    use crate::domain::McpServerSpec;
    use crate::gateway::{Gateway, GatewayFactory};
    use crate::harness::{Harness, HarnessFactory, HarnessSession, SessionCtx};
    use crate::Registries;
    use ryuzi_plugin_sdk::{AuthSpec, ModelDef, PluginManifest, ProviderMeta};
    use serde_json::json;
    use std::collections::BTreeMap;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

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
            surface: &crate::domain::Surface,
            _text: &str,
        ) -> anyhow::Result<crate::gateway::MessageRef> {
            Ok(crate::gateway::MessageRef {
                surface: surface.clone(),
                message_id: "m1".to_string(),
            })
        }
        async fn edit_status(
            &self,
            _msg: &crate::gateway::MessageRef,
            _text: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_result(
            &self,
            _surface: &crate::domain::Surface,
            _chunks: &[String],
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn post_error(
            &self,
            _surface: &crate::domain::Surface,
            _message: &str,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _s: &crate::domain::Surface,
            _r: &crate::domain::ApprovalRequest,
        ) -> anyhow::Result<crate::domain::ApprovalDecision> {
            Ok(crate::domain::ApprovalDecision::Cancel)
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
            slot: None,
            verified: false,
            experimental: false,
            auth: None,
            settings: vec![],
            mcp: vec![],
            extensions: vec![],
            skills: vec![],
            provider: None,
        }
    }

    fn harness_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: Some(Arc::new(FakeHarnessFactory)),
            gateway: None,
            connector: None,
            extension: None,
            provider: None,
            source: PluginSource::Builtin,
        }
    }

    fn gateway_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: Some(Arc::new(FakeGatewayFactory)),
            connector: None,
            extension: None,
            provider: None,
            source: PluginSource::Catalog,
        }
    }

    fn connector_only(id: &str) -> CorePlugin {
        CorePlugin {
            manifest: manifest(id),
            harness: None,
            gateway: None,
            connector: Some(Arc::new(FakeConnector)),
            extension: None,
            provider: None,
            source: PluginSource::SkillPack(std::path::PathBuf::from("/tmp/whatever")),
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
            extension: None,
            provider: None,
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
            extension: None,
            provider: None,
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
        assert_eq!(derive_kind(&connector_only("slack")), Some("skill-pack"));
        assert_eq!(derive_kind(&harness_only("native")), None);
    }

    #[test]
    fn derive_kind_integration_for_connector_without_skill_pack_source() {
        let mut plugin = connector_only("acme-conn");
        plugin.source = PluginSource::Catalog;
        assert_eq!(derive_kind(&plugin), Some("integration"));
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
        let store = std::sync::Arc::new(crate::Store::open(tmp.path()).await.unwrap());
        let plugin = gateway_only("bare-gateway");
        let ctx = InstalledCtx {
            installed_skills: vec![],
            installed_providers: vec![],
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

    #[tokio::test]
    async fn compute_installed_provider_follows_installed_set_without_connection() {
        // A default-installed provider with zero connections is "installed"
        // because it is in the persisted set; a provider that is neither seeded
        // nor connected is not.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::Store::open(tmp.path()).await.unwrap());
        crate::llm_router::installed::ensure_default_installed_providers(&store)
            .await
            .unwrap();
        let ctx = installed_ctx(&store).await.unwrap();

        // `anthropic` is in DEFAULT_INSTALLED; no connection exists.
        let anthropic = provider_only("anthropic");
        assert!(
            compute_installed(&store, &anthropic, "provider", false, false, &ctx)
                .await
                .unwrap(),
            "a default-installed provider is installed with zero connections"
        );

        // `xai` is not a default and has no connection.
        let xai = provider_only("xai");
        assert!(
            !compute_installed(&store, &xai, "provider", false, false, &ctx)
                .await
                .unwrap(),
            "a non-installed, connectionless provider is not installed"
        );
    }

    #[tokio::test]
    async fn compute_installed_provider_is_set_authoritative_ignoring_connections() {
        // Provider installed-ness is authoritative on the persisted set ONLY,
        // matching the Models list (which filters on the set). A family in the
        // set is installed; a family absent from the set is NOT installed even
        // when a connection row for it exists — so the Plugins card and the
        // Models list can never disagree.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::Store::open(tmp.path()).await.unwrap());

        // A live connection for `xai`, but `xai` is deliberately NOT in the set.
        crate::llm_router::connections::add_connection(
            &store,
            crate::llm_router::connections::ConnectionRow {
                id: "x1".into(),
                provider: "xai".into(),
                auth_type: "api_key".into(),
                label: "xAI".into(),
                priority: 0,
                enabled: true,
                data: Default::default(),
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();
        // `openai` is installed via the set alone (no connection needed).
        crate::llm_router::installed::install_provider(&store, "openai")
            .await
            .unwrap();

        let ctx = installed_ctx(&store).await.unwrap();

        let openai = provider_only("openai");
        assert!(
            compute_installed(&store, &openai, "provider", false, false, &ctx)
                .await
                .unwrap(),
            "a family in the installed set is installed"
        );

        let xai = provider_only("xai");
        assert!(
            !compute_installed(&store, &xai, "provider", false, false, &ctx)
                .await
                .unwrap(),
            "a family with a connection but absent from the set is NOT installed"
        );
    }

    // ---------- plugin_info ----------

    fn no_ctx(owns_slot: bool) -> PluginInfoContext<'static> {
        PluginInfoContext {
            install: None,
            remote: None,
            owns_slot,
        }
    }

    #[test]
    fn plugin_info_maps_identity_and_enabled_flag_through() {
        let plugin = harness_only("native");
        let info = plugin_info(&plugin, true, false, "integration", false, no_ctx(false));
        assert_eq!(info.id, "native");
        assert_eq!(info.name, "Plugin native");
        assert!(info.enabled);
        assert_eq!(info.source, "builtin");
        assert_eq!(info.capabilities, vec!["runtime".to_string()]);
        assert!(!info.configured);
        assert_eq!(info.kind, "integration");
        assert!(!info.installed);
        assert!(info.family.is_none());
        // No `plugin_installs` ledger row → ledger fields carry their defaults.
        assert!(!info.pinned);
        assert!(info.source_spec.is_none());
        // Builtin source, no cached remote-catalog row → all three enrichment
        // fields stay unset.
        assert!(info.catalog_source.is_none());
        assert!(info.catalog_version.is_none());
        assert!(info.blocked_reason.is_none());
        // No manifest `slot` claim → neither field is set.
        assert!(info.slot.is_none());
        assert!(!info.owns_slot);

        let info_disabled = plugin_info(&plugin, false, false, "integration", false, no_ctx(false));
        assert!(!info_disabled.enabled);
    }

    #[test]
    fn plugin_info_reports_slot_and_owns_slot() {
        let plugin = CorePlugin {
            manifest: PluginManifest {
                slot: Some("memory".to_string()),
                ..manifest("mem0")
            },
            ..harness_only("mem0")
        };
        let owner = plugin_info(&plugin, true, false, "integration", false, no_ctx(true));
        assert_eq!(owner.slot.as_deref(), Some("memory"));
        assert!(owner.owns_slot);

        let loser = plugin_info(&plugin, true, false, "integration", false, no_ctx(false));
        assert_eq!(
            loser.slot.as_deref(),
            Some("memory"),
            "the claim itself is still reported even when the plugin lost arbitration"
        );
        assert!(!loser.owns_slot);
    }

    // ---------- catalog_source_label ----------

    #[test]
    fn catalog_source_label_maps_catalog_sources_only() {
        assert_eq!(
            catalog_source_label(&PluginSource::Catalog).as_deref(),
            Some("embedded")
        );
        assert_eq!(
            catalog_source_label(&PluginSource::RemoteCatalog).as_deref(),
            Some("remote")
        );
        assert!(catalog_source_label(&PluginSource::Builtin).is_none());
        assert!(
            catalog_source_label(&PluginSource::SkillPack(std::path::PathBuf::from("/x")))
                .is_none()
        );
    }

    // ---------- remote-catalog enrichment (assemble_list) ----------

    // A plugin whose `CorePlugin.source` is `RemoteCatalog` (the merged-catalog
    // path a real daemon boot takes via `catalog::merged_catalog_plugins`) must
    // report `catalogSource: "remote"`, and a matching (blocked) cached row
    // must surface as `catalogVersion`/`blockedReason` on the SAME list entry —
    // exercising the `remote_catalog_index` lookup `assemble_list` builds once,
    // not a per-plugin query.
    #[tokio::test]
    async fn assemble_list_enriches_remote_catalog_plugin_with_blocked_reason() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::Store::open(tmp.path()).await.unwrap());
        let mut regs = Registries::new();
        let mut plugin = gateway_only("acme-remote");
        plugin.source = PluginSource::RemoteCatalog;
        regs.add_plugin(plugin);
        let cp = {
            let persistence = crate::agents::bootstrap::AgentPersistence::temporary(store.clone())
                .await
                .unwrap();
            ControlPlane::new(store, regs, persistence).await
        };

        cp.store()
            .upsert_remote_catalog(&[crate::store::RemoteCatalogRow {
                id: "acme-remote".to_string(),
                manifest_toml: String::new(),
                version: "2.0.0".to_string(),
                sequence: 1,
                blocked: true,
                blocked_reason: Some("revoked: CVE-2026-0001".to_string()),
                fetched_at: 0,
            }])
            .await
            .unwrap();

        let list = assemble_list(&cp).await.unwrap();
        let info = list
            .iter()
            .find(|p| p.id == "acme-remote")
            .expect("acme-remote present in the list");
        assert_eq!(info.catalog_source.as_deref(), Some("remote"));
        assert_eq!(info.catalog_version.as_deref(), Some("2.0.0"));
        assert_eq!(
            info.blocked_reason.as_deref(),
            Some("revoked: CVE-2026-0001")
        );
    }

    // An embedded-catalog plugin with NO matching cached remote row must still
    // report `catalogSource: "embedded"`, with the version/blocked fields left
    // unset — the remote cache only ever adds detail, never required for the
    // embedded label.
    #[tokio::test]
    async fn assemble_list_labels_embedded_catalog_plugin_without_remote_row() {
        let cp = test_cp().await;
        let list = assemble_list(&cp).await.unwrap();
        // `test_cp` registers the embedded catalog via `install_builtins`;
        // "notion" is a catalog-sourced integration (see
        // `plugin_info_configured_for_oauth_requires_stored_token_without_reconnect`).
        let notion = list.iter().find(|p| p.id == "notion").expect("notion");
        assert_eq!(notion.catalog_source.as_deref(), Some("embedded"));
        assert!(notion.catalog_version.is_none());
        assert!(notion.blocked_reason.is_none());
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
    fn field_kind_label_maps_every_variant() {
        assert_eq!(field_kind_label(FieldKind::String), "string");
        assert_eq!(field_kind_label(FieldKind::Int), "int");
        assert_eq!(field_kind_label(FieldKind::Bool), "bool");
    }

    // ---------- build_settings_info (Feature C3: kind/options/default) ----------

    #[tokio::test]
    async fn build_settings_info_carries_kind_options_and_default() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::Store::open(tmp.path()).await.unwrap());
        let fields = vec![
            SettingField {
                key: "plugin.acme.tier".to_string(),
                label: "Tier".to_string(),
                help: String::new(),
                secret: false,
                required: false,
                kind: FieldKind::String,
                options: vec!["free".to_string(), "pro".to_string()],
                default: Some("free".to_string()),
            },
            SettingField {
                key: "plugin.acme.retries".to_string(),
                label: "Retries".to_string(),
                help: String::new(),
                secret: false,
                required: false,
                kind: FieldKind::Int,
                options: vec![],
                default: Some("3".to_string()),
            },
            SettingField {
                key: "plugin.acme.verbose".to_string(),
                label: "Verbose".to_string(),
                help: String::new(),
                secret: false,
                required: false,
                kind: FieldKind::Bool,
                options: vec![],
                default: None,
            },
        ];

        let out = build_settings_info(&store, &fields).await.unwrap();
        assert_eq!(out.len(), 3);

        assert_eq!(out[0].kind, "string");
        assert_eq!(out[0].options, vec!["free".to_string(), "pro".to_string()]);
        assert_eq!(out[0].default.as_deref(), Some("free"));

        assert_eq!(out[1].kind, "int");
        assert!(out[1].options.is_empty());
        assert_eq!(out[1].default.as_deref(), Some("3"));

        assert_eq!(out[2].kind, "bool");
        assert!(out[2].options.is_empty());
        assert_eq!(out[2].default, None);
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
        let store = std::sync::Arc::new(crate::Store::open(tmp.path()).await.unwrap());
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
            Some(crate::plugins::oauth::pkce_challenge_s256("verifier-test-123").as_str())
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
        let store = std::sync::Arc::new(crate::Store::open(tmp.path()).await.unwrap());
        store
            .set_setting_raw("plugin.gw.client_id", "setting-client")
            .await
            .unwrap();

        // External plugin (no resource, no authorize_url): auth.setting IS the
        // client id key (google-workspace shape).
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
        let store = std::sync::Arc::new(crate::Store::open(tmp.path()).await.unwrap());
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
        let store = Arc::new(crate::Store::open(tmp.path()).await.unwrap());
        let mut regs = Registries::new();
        // Mirror the composition root: the `native` runtime is registered
        // explicitly before `install_builtins` adds providers, CLI agents, and
        // the catalog (see `install_builtins`'s doc — those builtins win
        // same-id collisions).
        regs.add_plugin(crate::harness::native::native_plugin());
        crate::plugins::install_builtins(&mut regs);
        {
            let persistence =
                crate::agents::bootstrap::AgentPersistence::temporary(Arc::clone(&store))
                    .await
                    .unwrap();
            ControlPlane::new(store, regs, persistence).await
        }
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
    async fn assemble_list_excludes_runtimes_and_synthesizes_curated_packs() {
        let cp = test_cp().await;
        let list = assemble_list(&cp).await.unwrap();
        assert!(list
            .iter()
            .all(|p| p.id != "native" && p.id != "claude-code"));
        assert!(list
            .iter()
            .any(|p| p.kind == "skill-pack" && p.id == "superpowers"));
        let anthropic = list.iter().find(|p| p.id == "anthropic").expect("provider");
        assert_eq!(anthropic.kind, "provider");
        assert_eq!(anthropic.family.as_deref(), Some("anthropic"));
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
        crate::llm_router::secrets::use_test_key_file();
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
        // capability): `is_enabled` always reports it enabled regardless of any
        // `plugin.<id>.enabled` write, so toggling it must error rather than
        // silently no-op (see `toggle_enabled`'s doc).
        let err = crate::plugins::toggle_enabled(cp.plugins(), &settings, "anthropic", true)
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
    async fn uninstall_provider_removes_every_family_connection() {
        let cp = test_cp().await;
        let now = crate::paths::now_ms();
        for (id, provider) in [
            ("c1", "anthropic"),
            ("c2", "anthropic-oauth"),
            ("c3", "openai"),
        ] {
            crate::llm_router::connections::add_connection(
                cp.store(),
                crate::llm_router::connections::ConnectionRow {
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

        let left = crate::llm_router::connections::list_connections(cp.store())
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
        crate::plugins::toggle_enabled(cp.plugins(), &settings, "github", true)
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

    // The positive skill-pack uninstall path (a real pack on disk removed via
    // `remove_installed_skill`) resolves through `InstallRoots::for_user()`,
    // i.e. the real user skills dir — environment-dependent — so only the
    // hermetic bail path is asserted here; the rest is covered by
    // `crate::skills_install` unit tests.

    #[tokio::test]
    async fn uninstall_skill_pack_unknown_id_errors() {
        let cp = test_cp().await;
        assert!(uninstall(&cp, "definitely-not-installed-pack")
            .await
            .is_err());
    }

    // ---------- begin_plugin_install resolution (steps 1-6) ----------

    /// Minimal hand-rolled HTTP mock on std::net. Serves the RFC 8414 root +
    /// path-inserted documents pointing endpoints (and, when
    /// `with_registration`, the registration endpoint) at itself, plus an RFC
    /// 7591 register endpoint; counts hits per route.
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
                    // Drain any request body so the client never sees a reset
                    // while still writing.
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

    async fn test_store() -> (crate::Store, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::Store::open(tmp.path()).await.unwrap();
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
        // google-workspace is the external-OAuth catalog plugin — its client id
        // key IS its auth.setting (validated write path).
        set_plugin_oauth_client_id(
            &cp,
            "google-workspace".to_string(),
            " google-client-1 ".to_string(),
        )
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
        set_plugin_oauth_client_id(&cp, "notion".to_string(), "notion-client-1".to_string())
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
        assert!(
            set_plugin_oauth_client_id(&cp, "notion".to_string(), "  ".to_string())
                .await
                .is_err()
        );
    }

    #[tokio::test]
    async fn drop_pending_plugin_flows_narrows_by_state_token_or_sweeps_the_plugin() {
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
        drop_pending_plugin_flows("wiz-cancel", Some("s1"));
        {
            let flows = plugin_oauth_flows()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner());
            assert!(!flows.contains_key(&plugin_oauth_flow_key("wiz-cancel", "s1")));
            assert!(flows.contains_key(&plugin_oauth_flow_key("wiz-cancel", "s2")));
        }
        drop_pending_plugin_flows("wiz-cancel", None);
        let flows = plugin_oauth_flows()
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        assert!(!flows.contains_key(&plugin_oauth_flow_key("wiz-cancel", "s2")));
    }

    #[tokio::test]
    async fn list_plugins_dispatches_as_array() {
        let s = state().await;
        let out = dispatch(&s, "list_plugins", json!({})).await.unwrap();
        assert!(out.is_array());
    }

    // ---------- component-plugin release management (Task 11a) ----------

    fn component_release(version: &str) -> crate::store::ComponentPluginReleaseRecord {
        crate::store::ComponentPluginReleaseRecord {
            plugin_id: "mimo".into(),
            version: version.into(),
            source_url: format!("https://feed.test/mimo/{version}"),
            sha256: "0".repeat(64),
            signing_key_id: "first-party".into(),
            installed_at: crate::paths::now_ms(),
            active: false,
            revoked: false,
            revocation_reason: None,
        }
    }

    #[tokio::test]
    async fn plugin_release_detail_lists_releases_and_active_version() {
        let cp = test_cp().await;
        for v in ["0.1.0", "0.2.0"] {
            cp.store()
                .upsert_component_release(&component_release(v))
                .await
                .unwrap();
        }
        cp.store()
            .set_active_component_release("mimo", "0.2.0")
            .await
            .unwrap();

        let detail = plugin_release_detail(&cp, "mimo").await.unwrap();
        assert_eq!(detail.plugin_id, "mimo");
        assert_eq!(detail.releases.len(), 2);
        assert_eq!(detail.active_version.as_deref(), Some("0.2.0"));
        assert!(detail
            .releases
            .iter()
            .any(|r| r.version == "0.2.0" && r.active));
        // Task 12: every release here was signed with the first-party test
        // fixture's key id ("first-party"), so `first_party` must be true for
        // all of them.
        assert!(detail.releases.iter().all(|r| r.first_party));
        // Task 12: no bundle is installed on disk in this test environment
        // (only the ledger row exists), so the manifest-derived permission
        // summary must be absent rather than guessed.
        assert!(detail.active_manifest.is_none());
    }

    // Task 12: a release signed by a key other than the first-party constant
    // must report `first_party: false` — the UI's publisher-verification
    // badge relies on this being computed server-side, never string-matched
    // client-side.
    #[tokio::test]
    async fn plugin_release_detail_marks_non_first_party_releases() {
        let cp = test_cp().await;
        let mut third_party = component_release("0.1.0");
        third_party.signing_key_id = "some-other-key".into();
        cp.store()
            .upsert_component_release(&third_party)
            .await
            .unwrap();

        let detail = plugin_release_detail(&cp, "mimo").await.unwrap();
        let release = detail.releases.first().unwrap();
        assert!(!release.first_party);
        assert_eq!(release.signing_key_id, "some-other-key");
    }

    // Task 12: a component id with no recorded releases at all (never
    // installed) must return an empty, well-formed detail rather than an
    // error — this is the shape Cockpit's PluginDetailView sees for a
    // never-installed component plugin.
    #[tokio::test]
    async fn plugin_release_detail_is_empty_for_a_never_installed_plugin() {
        let cp = test_cp().await;
        let detail = plugin_release_detail(&cp, "opencode").await.unwrap();
        assert_eq!(detail.plugin_id, "opencode");
        assert!(detail.releases.is_empty());
        assert!(detail.active_version.is_none());
        assert!(detail.active_manifest.is_none());
    }

    #[tokio::test]
    async fn rollback_component_plugin_revokes_bad_and_reactivates_prior_good() {
        let cp = test_cp().await;
        for v in ["0.1.0", "0.2.0"] {
            cp.store()
                .upsert_component_release(&component_release(v))
                .await
                .unwrap();
        }
        cp.store()
            .set_active_component_release("mimo", "0.2.0")
            .await
            .unwrap();

        let detail = rollback_component_plugin(&cp, "mimo", "0.2.0", "0.1.0")
            .await
            .unwrap();
        assert_eq!(detail.active_version.as_deref(), Some("0.1.0"));
        let bad = detail
            .releases
            .iter()
            .find(|r| r.version == "0.2.0")
            .unwrap();
        assert!(bad.revoked, "the bad version must be revoked");
        assert!(!bad.active);
        assert!(
            cp.plugins_restart_required(),
            "rollback must signal a host reload"
        );
    }

    // IMP-1: rollback whose target does NOT exist must be a clean no-op — the
    // bad version stays ACTIVE and un-revoked, never leaving the plugin with no
    // active release despite the RPC reporting failure.
    #[tokio::test]
    async fn rollback_is_a_no_op_when_target_version_is_missing() {
        let cp = test_cp().await;
        cp.store()
            .upsert_component_release(&component_release("0.2.0"))
            .await
            .unwrap();
        cp.store()
            .set_active_component_release("mimo", "0.2.0")
            .await
            .unwrap();

        match rollback_component_plugin(&cp, "mimo", "0.2.0", "9.9.9").await {
            Ok(_) => panic!("rollback to a missing target version must fail"),
            Err(err) => assert!(
                err.to_string().contains("no component release"),
                "unexpected error: {err}"
            ),
        }
        let active = cp
            .store()
            .active_component_release("mimo")
            .await
            .unwrap()
            .expect("the bad version must remain active after a failed rollback");
        assert_eq!(active.version, "0.2.0");
        assert!(
            !active.revoked,
            "the bad version must not have been revoked"
        );
    }

    // IMP-1: rollback to a REVOKED target is likewise a clean no-op.
    #[tokio::test]
    async fn rollback_is_a_no_op_when_target_version_is_revoked() {
        let cp = test_cp().await;
        for v in ["0.1.0", "0.2.0"] {
            cp.store()
                .upsert_component_release(&component_release(v))
                .await
                .unwrap();
        }
        cp.store()
            .set_active_component_release("mimo", "0.2.0")
            .await
            .unwrap();
        cp.store()
            .mark_component_release_revoked("mimo", "0.1.0", "bad")
            .await
            .unwrap();

        match rollback_component_plugin(&cp, "mimo", "0.2.0", "0.1.0").await {
            Ok(_) => panic!("rollback to a revoked target version must fail"),
            Err(err) => assert!(
                err.to_string().contains("revoked"),
                "unexpected error: {err}"
            ),
        }
        let active = cp
            .store()
            .active_component_release("mimo")
            .await
            .unwrap()
            .expect("the bad version must remain active after a failed rollback");
        assert_eq!(active.version, "0.2.0");
        assert!(
            !active.revoked,
            "the bad version must not have been revoked on a failed rollback"
        );
    }

    // With no first-party signing key configured yet (the all-zero
    // placeholder), an install must refuse fail-closed BEFORE any network I/O
    // rather than stage an unverifiable bundle.
    #[tokio::test]
    async fn install_component_plugin_is_fail_closed_without_a_signing_key() {
        let cp = test_cp().await;
        match install_component_plugin(&cp, "mimo", None).await {
            Ok(_) => panic!("expected a fail-closed refusal without a signing key"),
            Err(err) => assert!(
                err.to_string().contains("disabled until"),
                "unexpected error: {err}"
            ),
        }
    }

    #[tokio::test]
    async fn component_bootstrap_status_reports_pending_retry_until_completed() {
        let cp = test_cp().await;
        assert!(!component_bootstrap_status(&cp).await.unwrap().pending);

        cp.store()
            .set_setting_raw(
                crate::plugins::remote_catalog::FIRST_PARTY_BOOTSTRAP_RETRY,
                "download failed",
            )
            .await
            .unwrap();
        let pending = component_bootstrap_status(&cp).await.unwrap();
        assert!(pending.pending);
        assert_eq!(pending.message.as_deref(), Some("download failed"));

        // Completion clears the pending state even if the retry row lingers.
        cp.store()
            .set_setting_raw(
                crate::plugins::remote_catalog::FIRST_PARTY_BOOTSTRAP_MARKER,
                "1",
            )
            .await
            .unwrap();
        assert!(!component_bootstrap_status(&cp).await.unwrap().pending);
    }

    #[tokio::test]
    async fn component_bootstrap_status_dispatches() {
        let s = state().await;
        let out = dispatch(&s, "component_bootstrap_status", json!({}))
            .await
            .unwrap();
        assert_eq!(out["pending"], json!(false));
    }
}
