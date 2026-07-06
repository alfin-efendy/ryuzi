//! In-process LLM client seam.
//!
//! The axum endpoint server ([`super::server`]) exposes the router over
//! localhost HTTP for external agent processes. The native agent runtime
//! ([`crate::harness::native`]) needs the *same* routing, credential, OAuth
//! refresh, and format-translation behavior, but in-process — without a TCP
//! hop and without minting an endpoint key.
//!
//! This module owns the axum-free upstream path (moved verbatim out of
//! `server.rs`, retargeted from the server's private `AppState` to the public
//! [`UpstreamCtx`]) plus a streaming entry point,
//! [`anthropic_messages_stream`], that routes an Anthropic-Messages-format
//! request exactly like `/v1/messages` and yields Anthropic SSE events as
//! `(event_name, event_json)` pairs regardless of the upstream provider's
//! native wire format.

use crate::llm_router::registry::{self, ApiFormat, AuthScheme, ProviderDescriptor};
use crate::llm_router::{capabilities, claude_cloak, connections, oauth, routes, translate};
use crate::store::Store;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

/// Everything the upstream path needs, decoupled from axum. Cheap to clone
/// (an `Arc<Store>`, a `reqwest::Client` which is internally reference
/// counted, and a small `Option<String>`).
#[derive(Clone)]
pub struct UpstreamCtx {
    pub store: Arc<Store>,
    pub http: reqwest::Client,
    /// Test-only override for the OAuth token endpoint used by the reactive
    /// (post-401) refresh path. `None` in production, which uses each
    /// provider's static `registry::oauth_config` token_url.
    pub oauth_token_url_override: Option<String>,
}

impl UpstreamCtx {
    /// Construct a production context (no OAuth token-url override).
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            http: reqwest::Client::new(),
            oauth_token_url_override: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Model routing (moved from server.rs — behavior unchanged)
// ---------------------------------------------------------------------------

pub struct RouteTarget {
    pub conn: connections::ConnectionRow,
    pub desc: &'static ProviderDescriptor,
    pub upstream_model: String,
}

pub async fn route_model(store: &Store, requested: &str) -> anyhow::Result<Option<RouteTarget>> {
    Ok(route_models_for_body(store, requested, None)
        .await?
        .into_iter()
        .next())
}

pub async fn route_model_for_body(
    store: &Store,
    requested: &str,
    body: Option<&Value>,
) -> anyhow::Result<Option<RouteTarget>> {
    Ok(route_models_for_body(store, requested, body)
        .await?
        .into_iter()
        .next())
}

pub async fn route_model_for_anthropic_messages(
    store: &Store,
    requested: &str,
) -> anyhow::Result<Option<RouteTarget>> {
    Ok(route_models_for_anthropic_messages(store, requested)
        .await?
        .into_iter()
        .next())
}

pub async fn route_models_for_body(
    store: &Store,
    requested: &str,
    body: Option<&Value>,
) -> anyhow::Result<Vec<RouteTarget>> {
    route_models_for_body_matching(store, requested, body, |_, _| true).await
}

pub async fn route_models_for_anthropic_messages(
    store: &Store,
    requested: &str,
) -> anyhow::Result<Vec<RouteTarget>> {
    route_models_for_body_matching(store, requested, None, anthropic_messages_target_allowed).await
}

async fn route_models_for_body_matching(
    store: &Store,
    requested: &str,
    body: Option<&Value>,
    target_allowed: fn(&connections::ConnectionRow, &ProviderDescriptor) -> bool,
) -> anyhow::Result<Vec<RouteTarget>> {
    let conns = connections::list_connections(store).await?;
    let enabled: Vec<_> = conns.into_iter().filter(|c| c.enabled).collect();
    let required = body
        .map(capabilities::required_capabilities_from_body)
        .unwrap_or_default();
    if let Some((prov, model)) = requested.split_once('/') {
        let candidates: Vec<_> = enabled
            .into_iter()
            .filter(|conn| {
                conn.provider == prov
                    && registry::descriptor(&conn.provider)
                        .map(|desc| {
                            target_allowed(conn, desc)
                                && connection_has_required_credentials(desc, conn)
                                && connection_serves_model(desc, conn, model, true)
                        })
                        .unwrap_or(false)
            })
            .collect();
        let mut out = Vec::new();
        for conn in ordered_provider_connections(store, prov, model, candidates).await? {
            if let Some(desc) = registry::descriptor(&conn.provider) {
                out.push(RouteTarget {
                    conn,
                    desc,
                    upstream_model: model.to_string(),
                });
            }
        }
        return Ok(out);
    }
    let route_list = routes::list_model_routes(store).await?;
    if let Some(route) = routes::route_by_name(&route_list, requested) {
        let targets =
            prefer_capable_targets(routes::ordered_targets(store, route).await?, required);
        let mut out = Vec::new();
        let mut seen = std::collections::HashSet::<(String, String)>::new();
        for target in targets {
            for route_target in
                expanded_route_targets(store, &enabled, &target, target_allowed).await?
            {
                let key = (
                    route_target.conn.id.clone(),
                    route_target.upstream_model.clone(),
                );
                if !seen.insert(key) {
                    continue;
                }
                out.push(route_target);
            }
        }
        return Ok(out);
    }
    // Bare model: first (highest-priority) connection listing it.
    let mut provider_order = Vec::<String>::new();
    let mut grouped = std::collections::BTreeMap::<String, Vec<connections::ConnectionRow>>::new();
    for conn in enabled {
        let Some(desc) = registry::descriptor(&conn.provider) else {
            continue;
        };
        if target_allowed(&conn, desc)
            && connection_has_required_credentials(desc, &conn)
            && connection_serves_model(desc, &conn, requested, false)
        {
            if !grouped.contains_key(&conn.provider) {
                provider_order.push(conn.provider.clone());
            }
            grouped.entry(conn.provider.clone()).or_default().push(conn);
        }
    }
    let mut out = Vec::new();
    for provider in provider_order {
        let candidates = grouped.remove(&provider).unwrap_or_default();
        for conn in ordered_provider_connections(store, &provider, requested, candidates).await? {
            if let Some(desc) = registry::descriptor(&conn.provider) {
                out.push(RouteTarget {
                    conn,
                    desc,
                    upstream_model: requested.to_string(),
                });
            }
        }
    }
    Ok(out)
}

fn anthropic_messages_target_allowed(
    conn: &connections::ConnectionRow,
    _desc: &ProviderDescriptor,
) -> bool {
    conn.provider != "openai-oauth"
}

async fn expanded_route_targets(
    store: &Store,
    enabled: &[connections::ConnectionRow],
    target: &routes::ModelRouteTarget,
    target_allowed: fn(&connections::ConnectionRow, &ProviderDescriptor) -> bool,
) -> anyhow::Result<Vec<RouteTarget>> {
    let Some(primary) = enabled.iter().find(|conn| conn.id == target.connection_id) else {
        return Ok(Vec::new());
    };
    let Some(primary_desc) = registry::descriptor(&primary.provider) else {
        return Ok(Vec::new());
    };
    if !target_allowed(primary, primary_desc)
        || !connection_serves_model(primary_desc, primary, &target.model, false)
    {
        return Ok(Vec::new());
    }

    let mut candidates = Vec::new();
    for conn in std::iter::once(primary).chain(
        enabled
            .iter()
            .filter(|conn| conn.id != primary.id && conn.provider == primary.provider),
    ) {
        let Some(desc) = registry::descriptor(&conn.provider) else {
            continue;
        };
        if target_allowed(conn, desc)
            && connection_has_required_credentials(desc, conn)
            && connection_serves_model(desc, conn, &target.model, false)
        {
            candidates.push(conn.clone());
        }
    }

    let ordered =
        ordered_provider_connections(store, &primary.provider, &target.model, candidates).await?;
    Ok(ordered
        .into_iter()
        .filter_map(|conn| {
            registry::descriptor(&conn.provider).map(|desc| RouteTarget {
                conn,
                desc,
                upstream_model: target.model.clone(),
            })
        })
        .collect())
}

fn route_target_has_candidate(
    enabled: &[connections::ConnectionRow],
    target: &routes::ModelRouteTarget,
    target_allowed: fn(&connections::ConnectionRow, &ProviderDescriptor) -> bool,
) -> bool {
    let Some(primary) = enabled.iter().find(|conn| conn.id == target.connection_id) else {
        return false;
    };
    let Some(primary_desc) = registry::descriptor(&primary.provider) else {
        return false;
    };
    if !target_allowed(primary, primary_desc)
        || !connection_serves_model(primary_desc, primary, &target.model, false)
    {
        return false;
    }

    enabled.iter().any(|conn| {
        conn.provider == primary.provider
            && registry::descriptor(&conn.provider)
                .map(|desc| {
                    target_allowed(conn, desc)
                        && connection_has_required_credentials(desc, conn)
                        && connection_serves_model(desc, conn, &target.model, false)
                })
                .unwrap_or(false)
    })
}

fn connection_has_required_credentials(
    desc: &ProviderDescriptor,
    conn: &connections::ConnectionRow,
) -> bool {
    if desc.no_auth || desc.auth == AuthScheme::None {
        return true;
    }
    if connections::is_oauth(conn) {
        return conn.data.needs_relogin != Some(true)
            && (has_text(conn.data.access_token.as_deref())
                || has_text(conn.data.refresh_token.as_deref())
                || has_text(conn.data.api_key.as_deref()));
    }
    has_text(conn.data.api_key.as_deref())
}

fn has_text(value: Option<&str>) -> bool {
    value.map(|s| !s.trim().is_empty()).unwrap_or(false)
}

fn connection_serves_model(
    desc: &ProviderDescriptor,
    conn: &connections::ConnectionRow,
    model: &str,
    allow_unlisted: bool,
) -> bool {
    let models = connections::effective_models(desc, conn);
    (allow_unlisted && models.is_empty()) || models.iter().any(|m| m == model)
}

async fn ordered_provider_connections(
    store: &Store,
    provider: &str,
    scope: &str,
    candidates: Vec<connections::ConnectionRow>,
) -> anyhow::Result<Vec<connections::ConnectionRow>> {
    if candidates.len() <= 1 {
        return Ok(candidates);
    }
    let ids = candidates
        .iter()
        .map(|conn| conn.id.clone())
        .collect::<Vec<_>>();
    let ordered_ids = routes::ordered_provider_connection_ids(store, provider, scope, &ids).await?;
    let mut by_id = candidates
        .into_iter()
        .map(|conn| (conn.id.clone(), conn))
        .collect::<std::collections::HashMap<_, _>>();
    Ok(ordered_ids
        .into_iter()
        .filter_map(|id| by_id.remove(&id))
        .collect())
}

fn prefer_capable_targets(
    targets: Vec<routes::ModelRouteTarget>,
    required: capabilities::RequiredCapabilities,
) -> Vec<routes::ModelRouteTarget> {
    if !required.any() || targets.len() <= 1 {
        return targets;
    }
    let mut capable = Vec::new();
    let mut rest = Vec::new();
    for target in targets {
        if required.satisfied_by(capabilities::model_capabilities(&target.model)) {
            capable.push(target);
        } else {
            rest.push(target);
        }
    }
    if capable.is_empty() {
        rest
    } else {
        capable.extend(rest);
        capable
    }
}

/// The first model of the highest-priority enabled connection, as a
/// `provider/model` id — used as a fallback when no model is configured.
/// `None` if no enabled connection offers any model.
pub async fn default_model(store: &Store) -> Option<String> {
    let conns = connections::list_connections(store).await.ok()?;
    for conn in conns.into_iter().filter(|c| c.enabled) {
        let Some(desc) = registry::descriptor(&conn.provider) else {
            continue;
        };
        if !connection_has_required_credentials(desc, &conn) {
            continue;
        }
        if let Some(model) = connections::effective_models(desc, &conn)
            .into_iter()
            .next()
        {
            return Some(format!("{}/{}", conn.provider, model));
        }
    }
    None
}

/// Default model for the native runtime / Anthropic Messages client path.
/// Prefer named routes so user-created combo aliases become the natural native
/// default, but skip `openai-oauth` because it only accepts Responses wire.
pub async fn default_anthropic_messages_model(store: &Store) -> Option<String> {
    let conns = connections::list_connections(store).await.ok()?;
    let enabled: Vec<_> = conns.into_iter().filter(|c| c.enabled).collect();
    if let Ok(route_list) = routes::list_model_routes(store).await {
        for route in route_list
            .into_iter()
            .filter(|r| r.enabled && !r.targets.is_empty())
        {
            let compatible = route.targets.iter().any(|target| {
                route_target_has_candidate(&enabled, target, anthropic_messages_target_allowed)
            });
            if compatible {
                return Some(route.name);
            }
        }
    }
    for conn in enabled {
        let Some(desc) = registry::descriptor(&conn.provider) else {
            continue;
        };
        if !anthropic_messages_target_allowed(&conn, desc) {
            continue;
        }
        if !connection_has_required_credentials(desc, &conn) {
            continue;
        }
        if let Some(model) = connections::effective_models(desc, &conn)
            .into_iter()
            .next()
        {
            return Some(format!("{}/{}", conn.provider, model));
        }
    }
    None
}

/// Whether the in-process native client can actually drive `conn` on the
/// Anthropic-Messages path. Reachable in-process:
///   * any api-key / no-auth connection (generic `/messages` or `/chat/
///     completions` wiring in [`upstream_request`], with OpenAI↔Anthropic
///     translation),
///   * the `anthropic-oauth` Claude subscription, and
///   * `kiro` (AWS CodeWhisperer) via [`kiro_stream`] — the same
///     EventStream→OpenAI→Anthropic translation the endpoint server uses.
/// The one provider still NOT drivable natively is `openai-oauth` (Codex),
/// whose Responses API needs an Anthropic→Responses request translation the
/// native path doesn't build yet. Listing a Codex model in the picker would
/// be a mockup that errors on send, so it's excluded here.
fn native_client_can_drive(conn: &connections::ConnectionRow) -> bool {
    if !connections::is_oauth(conn) {
        return true;
    }
    matches!(conn.provider.as_str(), "anthropic-oauth" | "kiro")
}

/// The models a native session can actually be pointed at, in the order the
/// UI should present them: enabled named routes (combos) that have at least
/// one natively-drivable target, then every `provider/model` served by an
/// enabled, credentialed, natively-drivable connection.
///
/// This is the source of truth for the Native runtime card + composer model
/// picker — the Native catalog entry has an empty model list on purpose
/// ("models come from connections"), so without this the picker was empty and
/// users could never pin a native model (hence it "always went to the Claude
/// subscription" default). Only models that will actually run are listed, so
/// the picker never offers a Codex/Kiro entry that would error on send.
pub async fn selectable_native_models(store: &Store) -> Vec<String> {
    let mut out = Vec::<String>::new();
    let mut seen = std::collections::HashSet::<String>::new();
    let push = |m: String, out: &mut Vec<String>, seen: &mut std::collections::HashSet<String>| {
        if seen.insert(m.clone()) {
            out.push(m);
        }
    };

    let enabled: Vec<connections::ConnectionRow> = match connections::list_connections(store).await
    {
        Ok(conns) => conns.into_iter().filter(|c| c.enabled).collect(),
        Err(_) => Vec::new(),
    };

    // Routes first — but only if at least one target is drivable in-process,
    // so a Codex-only or keyless route isn't offered as a native model.
    if let Ok(routes) = routes::list_model_routes(store).await {
        for route in routes
            .into_iter()
            .filter(|r| r.enabled && !r.targets.is_empty())
        {
            let usable = route.targets.iter().any(|target| {
                enabled
                    .iter()
                    .find(|c| c.id == target.connection_id)
                    .is_some_and(|c| {
                        native_client_can_drive(c)
                            && registry::descriptor(&c.provider).is_some_and(|desc| {
                                connection_has_required_credentials(desc, c)
                                    && connection_serves_model(desc, c, &target.model, false)
                            })
                    })
            });
            if usable {
                push(route.name, &mut out, &mut seen);
            }
        }
    }

    for conn in &enabled {
        let Some(desc) = registry::descriptor(&conn.provider) else {
            continue;
        };
        if !native_client_can_drive(conn) || !connection_has_required_credentials(desc, conn) {
            continue;
        }
        for model in connections::effective_models(desc, conn) {
            push(format!("{}/{}", conn.provider, model), &mut out, &mut seen);
        }
    }

    out
}

/// Anthropic's Claude-Code-branded system prefix, required by the
/// anthropic-oauth (Claude subscription) upstream — that's the wire's own
/// contract for the OAuth token, not a client-visible feature; no other
/// header/body cloaking is added (spec #2).
/// Ensure the outgoing Anthropic `system` field begins with the Claude-Code
/// prefix block: a bare string is wrapped into `[prefix, {type:text,text:the
/// old string}]`; an array gets the prefix prepended (skipped if already
/// present); an absent/other value is replaced with `[prefix]`.
fn inject_claude_system_prompt(body: &mut Value) {
    crate::llm_router::models::inject_claude_code_system_prompt(body);
}

fn claude_cloak_map_for(target: &RouteTarget, body: &Value) -> claude_cloak::ToolNameMap {
    claude_cloak::tool_name_map_for(&target.conn.provider, &target.conn.data, body)
}

// ---------------------------------------------------------------------------
// Upstream request construction (moved from server.rs — behavior unchanged,
// `&AppState` retargeted to `&UpstreamCtx`)
// ---------------------------------------------------------------------------

pub(crate) fn upstream_request(
    ctx: &UpstreamCtx,
    target: &RouteTarget,
    body: &Value,
) -> anyhow::Result<reqwest::RequestBuilder> {
    if connections::is_oauth(&target.conn) {
        return oauth_upstream_request(ctx, target, body);
    }
    if target.desc.no_auth {
        return free_upstream_request(ctx, target, body);
    }
    let base = connections::effective_base_url(target.desc, &target.conn)
        .ok_or_else(|| anyhow::anyhow!("connection {} has no base URL", target.conn.id))?;
    let path = match target.desc.format {
        ApiFormat::OpenAi => "/chat/completions",
        ApiFormat::Anthropic => "/messages",
    };
    let mut req = ctx.http.post(format!("{base}{path}")).json(body);
    let key = target.conn.data.api_key.clone().unwrap_or_default();
    req = match target.desc.auth {
        AuthScheme::XApiKey => req
            .header("x-api-key", key)
            .header("anthropic-version", "2023-06-01"),
        AuthScheme::Bearer => req.header("authorization", format!("Bearer {key}")),
        AuthScheme::None => req,
    };
    Ok(req)
}

/// OAuth-authenticated upstream request (anthropic-oauth or openai-oauth).
/// The credential is ALWAYS `data.access_token` — never `data.api_key`,
/// which oauth connections don't populate.
fn oauth_upstream_request(
    ctx: &UpstreamCtx,
    target: &RouteTarget,
    body: &Value,
) -> anyhow::Result<reqwest::RequestBuilder> {
    let access_token = target.conn.data.access_token.clone().unwrap_or_default();
    match target.conn.provider.as_str() {
        "anthropic-oauth" => {
            // Same base-resolution as the api-key path (honors a per-connection
            // override, which is how tests point this at a mock upstream) —
            // in production that's just the descriptor's real Anthropic base.
            let base = connections::effective_base_url(target.desc, &target.conn)
                .ok_or_else(|| anyhow::anyhow!("connection {} has no base URL", target.conn.id))?;
            let mut anthropic_body = body.clone();
            inject_claude_system_prompt(&mut anthropic_body);
            let session_id = uuid::Uuid::new_v4().to_string();
            let cloaked = claude_cloak::enabled(&target.conn.data);
            if cloaked {
                claude_cloak::apply_request_cloak(&mut anthropic_body, &access_token, &session_id);
            }
            let req = ctx
                .http
                .post(format!("{base}/messages?beta=true"))
                .json(&anthropic_body)
                .header("authorization", format!("Bearer {access_token}"))
                .header("anthropic-version", "2023-06-01")
                .header(
                    "anthropic-beta",
                    crate::llm_router::models::ANTHROPIC_OAUTH_BETA,
                )
                .header("anthropic-dangerous-direct-browser-access", "true")
                .header("user-agent", "claude-cli/2.1.92 (external, sdk-cli)")
                .header("x-app", "cli")
                .header("x-stainless-helper-method", "stream")
                .header("x-stainless-retry-count", "0");
            Ok(if cloaked {
                claude_cloak::spoof_headers(req, &session_id)
            } else {
                req
            })
        }
        "openai-oauth" => {
            // Codex's Responses wire is a fixed protocol endpoint, distinct
            // from the descriptor's placeholder `base_url` (see the NOTE on
            // the `openai-oauth` catalog entry in registry.rs) — Codex CLI
            // never talks to anything else, so this isn't override-able.
            const CODEX_BASE: &str = "https://chatgpt.com/backend-api/codex";
            let mut req = ctx
                .http
                .post(format!("{CODEX_BASE}/responses"))
                .json(body)
                .header("authorization", format!("Bearer {access_token}"))
                .header("originator", "codex_cli_rs")
                .header("session_id", uuid::Uuid::new_v4().to_string());
            if let Some(account_id) = target
                .conn
                .data
                .provider_specific
                .as_ref()
                .and_then(|v| v.get("chatgpt_account_id"))
                .and_then(|v| v.as_str())
            {
                req = req.header("chatgpt-account-id", account_id);
            }
            Ok(req)
        }
        other => Err(anyhow::anyhow!(
            "no OAuth upstream wiring for provider `{other}`"
        )),
    }
}

/// Free-tier passthrough (opencode-free): no real credential, just the
/// wire's own placeholder bearer + client-id header.
fn free_upstream_request(
    ctx: &UpstreamCtx,
    target: &RouteTarget,
    body: &Value,
) -> anyhow::Result<reqwest::RequestBuilder> {
    let base = connections::effective_base_url(target.desc, &target.conn)
        .ok_or_else(|| anyhow::anyhow!("connection {} has no base URL", target.conn.id))?;
    let path = match target.desc.format {
        ApiFormat::OpenAi => "/chat/completions",
        ApiFormat::Anthropic => "/messages",
    };
    Ok(ctx
        .http
        .post(format!("{base}{path}"))
        .json(body)
        .header("authorization", "Bearer public")
        .header("x-opencode-client", "desktop"))
}

/// Build + send the upstream request; on a 401/403 from an OAuth-backed
/// target, refresh once via `force_refresh` and retry the same request. The
/// retry is kept only if the refresh itself succeeds — a failed refresh
/// falls through to the original (failed) response so the caller's normal
/// error handling still fires. This covers non-stream calls directly and
/// gives streaming calls a pre-stream retry (called before any response
/// bytes are read); a 401 that arrives mid-stream is NOT retried.
pub(crate) async fn send_upstream(
    ctx: &UpstreamCtx,
    target: &mut RouteTarget,
    body: &Value,
) -> anyhow::Result<reqwest::Response> {
    let resp = upstream_request(ctx, target, body)?.send().await?;
    if matches!(resp.status().as_u16(), 401 | 403) && connections::is_oauth(&target.conn) {
        let refreshed = match &ctx.oauth_token_url_override {
            Some(token_url) => oauth::refresh::force_refresh_with_token_url(
                &ctx.store,
                &ctx.http,
                &mut target.conn,
                token_url,
            )
            .await
            .is_ok(),
            None => oauth::refresh::force_refresh(&ctx.store, &ctx.http, &mut target.conn)
                .await
                .is_ok(),
        };
        if refreshed {
            return Ok(upstream_request(ctx, target, body)?.send().await?);
        }
    }
    Ok(resp)
}

#[derive(Debug, Clone)]
struct UpstreamAttemptFailure {
    provider: String,
    message: String,
    status: Option<u16>,
}

impl UpstreamAttemptFailure {
    fn display(&self) -> String {
        format!("[{}] {}", self.provider, self.message)
    }
}

fn should_try_next_target(failure: &UpstreamAttemptFailure) -> bool {
    let status_retryable = matches!(
        failure.status,
        Some(401 | 403 | 408 | 409 | 425 | 429 | 500..=599)
    );
    if status_retryable {
        return true;
    }
    let msg = failure.message.to_ascii_lowercase();
    [
        "quota",
        "usage",
        "rate limit",
        "rate_limit",
        "overloaded",
        "capacity",
        "insufficient",
        "exceeded",
        "expired",
        "reconnect",
    ]
    .iter()
    .any(|needle| msg.contains(needle))
}

fn fallback_error(requested: &str, failures: &[UpstreamAttemptFailure]) -> anyhow::Error {
    if failures.is_empty() {
        return anyhow::anyhow!("no enabled connection serves model '{requested}'");
    }
    if failures.len() == 1 {
        return anyhow::anyhow!(failures[0].display());
    }
    anyhow::anyhow!(
        "all fallback targets failed for model '{requested}': {}",
        failures
            .iter()
            .map(UpstreamAttemptFailure::display)
            .collect::<Vec<_>>()
            .join("; ")
    )
}

async fn ensure_fresh_for_attempt(
    ctx: &UpstreamCtx,
    target: &mut RouteTarget,
) -> Result<(), UpstreamAttemptFailure> {
    if oauth::refresh::ensure_fresh(&ctx.store, &ctx.http, &mut target.conn)
        .await
        .is_err()
        && target.conn.data.needs_relogin == Some(true)
    {
        return Err(UpstreamAttemptFailure {
            provider: target.conn.provider.clone(),
            message: format!(
                "the {} connection needs to be reconnected — its login session has expired",
                target.conn.provider
            ),
            status: Some(401),
        });
    }
    Ok(())
}

async fn upstream_status_failure(
    provider: String,
    resp: reqwest::Response,
) -> UpstreamAttemptFailure {
    let status = resp.status().as_u16();
    // Read the body as text first so non-JSON errors (Kiro/AWS return an
    // `{"message":...}` or `__type` shape, or plain text) aren't discarded as
    // a generic "upstream error". Try the common JSON error shapes, then fall
    // back to a trimmed raw-body snippet.
    let body = resp.text().await.unwrap_or_default();
    let message = extract_upstream_error_message(&body);
    UpstreamAttemptFailure {
        provider,
        message,
        status: Some(status),
    }
}

/// Pull a human-readable error out of an upstream error body across the shapes
/// providers actually use: Anthropic/OpenAI `{"error":{"message"}}`, a bare
/// `{"error":"..."}`, AWS/Kiro `{"message":...}` / `{"Message":...}`, else a
/// trimmed snippet of the raw body (never just "upstream error" when the
/// upstream actually said something).
fn extract_upstream_error_message(body: &str) -> String {
    if let Ok(v) = serde_json::from_str::<Value>(body) {
        for path in [
            &v["error"]["message"],
            &v["error"],
            &v["message"],
            &v["Message"],
        ] {
            if let Some(s) = path.as_str() {
                if !s.trim().is_empty() {
                    return s.to_string();
                }
            }
        }
    }
    let trimmed = body.trim();
    if trimmed.is_empty() {
        "upstream error".to_string()
    } else {
        trimmed.chars().take(300).collect()
    }
}

// ---------------------------------------------------------------------------
// In-process streaming seam
// ---------------------------------------------------------------------------

/// One Anthropic-format SSE event: `(event_name, event_json)`. For example
/// `("content_block_delta", {"type":"content_block_delta","index":0,
/// "delta":{"type":"text_delta","text":"hi"}})`.
pub type AnthropicEvent = (String, Value);

/// A decoded Anthropic streaming event. The native runner matches on this
/// instead of string-matching event names + poking at `Value`.
#[derive(Debug, Clone, PartialEq)]
pub enum MessageStreamEvent {
    MessageStart(Value),
    ContentBlockStart {
        index: i64,
        block: Value,
    },
    TextDelta {
        index: i64,
        text: String,
    },
    ThinkingDelta {
        index: i64,
        text: String,
    },
    InputJsonDelta {
        index: i64,
        partial_json: String,
    },
    ContentBlockStop {
        index: i64,
    },
    MessageDelta {
        stop_reason: Option<String>,
        output_tokens: i64,
    },
    MessageStop,
    Error(String),
}

impl MessageStreamEvent {
    /// Decode a raw `(event_name, data)` pair. Returns `None` for events the
    /// runner does not need (e.g. `ping`).
    pub fn from_event(ev: &AnthropicEvent) -> Option<Self> {
        let (name, data) = ev;
        let index = data.get("index").and_then(|v| v.as_i64()).unwrap_or(0);
        match name.as_str() {
            "message_start" => Some(MessageStreamEvent::MessageStart(
                data.get("message").cloned().unwrap_or(Value::Null),
            )),
            "content_block_start" => Some(MessageStreamEvent::ContentBlockStart {
                index,
                block: data.get("content_block").cloned().unwrap_or(Value::Null),
            }),
            "content_block_delta" => {
                let delta = data.get("delta")?;
                match delta.get("type").and_then(|v| v.as_str()) {
                    Some("text_delta") => Some(MessageStreamEvent::TextDelta {
                        index,
                        text: delta
                            .get("text")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    }),
                    Some("thinking_delta") => Some(MessageStreamEvent::ThinkingDelta {
                        index,
                        text: delta
                            .get("thinking")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    }),
                    Some("input_json_delta") => Some(MessageStreamEvent::InputJsonDelta {
                        index,
                        partial_json: delta
                            .get("partial_json")
                            .and_then(|v| v.as_str())
                            .unwrap_or_default()
                            .to_string(),
                    }),
                    _ => None,
                }
            }
            "content_block_stop" => Some(MessageStreamEvent::ContentBlockStop { index }),
            "message_delta" => Some(MessageStreamEvent::MessageDelta {
                stop_reason: data
                    .get("delta")
                    .and_then(|d| d.get("stop_reason"))
                    .and_then(|v| v.as_str())
                    .map(|s| s.to_string()),
                output_tokens: data
                    .get("usage")
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(|v| v.as_i64())
                    .unwrap_or(0),
            }),
            "message_stop" => Some(MessageStreamEvent::MessageStop),
            "error" => Some(MessageStreamEvent::Error(
                data.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("upstream error")
                    .to_string(),
            )),
            _ => None,
        }
    }
}

/// Route + send an Anthropic-Messages-format request exactly like
/// `/v1/messages` (stream forced on) and yield Anthropic SSE events. The
/// returned channel closes when the upstream stream ends. Both Anthropic-
/// format upstreams (events forwarded directly) and OpenAI-format upstreams
/// (request translated, chunks re-encoded via
/// [`translate::OpenAiToAnthropicStream`]) produce the same event shape.
///
/// Errors surfaced BEFORE streaming (routing miss, dead OAuth token, a non-2xx
/// upstream status) fail the call directly; a mid-stream transport error is
/// delivered as a trailing `Err` in the channel.
pub async fn anthropic_messages_stream(
    ctx: &UpstreamCtx,
    body: Value,
) -> anyhow::Result<mpsc::Receiver<anyhow::Result<AnthropicEvent>>> {
    let requested = body["model"].as_str().unwrap_or("").to_string();
    let targets = route_models_for_anthropic_messages(&ctx.store, &requested).await?;
    if targets.is_empty() {
        anyhow::bail!("no enabled connection serves model '{requested}'");
    }

    let mut failures = Vec::new();
    for mut target in targets {
        if target.conn.provider == "openai-oauth" {
            failures.push(UpstreamAttemptFailure {
                provider: target.conn.provider.clone(),
                message: "the OpenAI (ChatGPT) connection speaks the Responses API and cannot serve the native runtime yet".into(),
                status: Some(400),
            });
            continue;
        }
        if let Err(failure) = ensure_fresh_for_attempt(ctx, &mut target).await {
            let try_next = should_try_next_target(&failure);
            failures.push(failure);
            if try_next {
                continue;
            }
            return Err(fallback_error(&requested, &failures));
        }

        let mut attempt_body = body.clone();
        attempt_body["model"] = json!(target.upstream_model);
        attempt_body["stream"] = json!(true);

        let started = crate::paths::now_ms();

        // Kiro has its own AWS-EventStream upstream (not the generic
        // `/messages` or `/chat/completions` path), so it's handled before the
        // format match. On a non-2xx it records a failure and tries the next
        // fallback target, matching the other providers' behavior.
        if target.conn.provider == "kiro" {
            match kiro_stream(ctx, &mut target, &attempt_body, started).await {
                Ok(rx) => return Ok(rx),
                Err(failure) => {
                    let try_next = should_try_next_target(&failure);
                    failures.push(failure);
                    if try_next {
                        continue;
                    }
                    return Err(fallback_error(&requested, &failures));
                }
            }
        }

        let conn_id = target.conn.id.clone();
        let provider = target.conn.provider.clone();
        let upstream_model = target.upstream_model.clone();
        let (tx, rx) = mpsc::channel::<anyhow::Result<AnthropicEvent>>(64);

        match target.desc.format {
            ApiFormat::Anthropic => {
                let tool_map = claude_cloak_map_for(&target, &attempt_body);
                let resp = send_upstream(ctx, &mut target, &attempt_body).await?;
                if !resp.status().is_success() {
                    let failure = upstream_status_failure(provider, resp).await;
                    let try_next = should_try_next_target(&failure);
                    failures.push(failure);
                    if try_next {
                        continue;
                    }
                    return Err(fallback_error(&requested, &failures));
                }
                let store = ctx.store.clone();
                tokio::spawn(async move {
                    pump_anthropic(
                        resp,
                        tx,
                        store,
                        conn_id,
                        provider,
                        upstream_model,
                        started,
                        tool_map,
                    )
                    .await;
                });
                return Ok(rx);
            }
            ApiFormat::OpenAi => {
                let upstream_body = translate::anthropic_to_openai_request(&attempt_body)?;
                let resp = send_upstream(ctx, &mut target, &upstream_body).await?;
                if !resp.status().is_success() {
                    let failure = upstream_status_failure(provider, resp).await;
                    let try_next = should_try_next_target(&failure);
                    failures.push(failure);
                    if try_next {
                        continue;
                    }
                    return Err(fallback_error(&requested, &failures));
                }
                let store = ctx.store.clone();
                let model = upstream_model.clone();
                tokio::spawn(async move {
                    pump_openai_translated(
                        resp,
                        model,
                        tx,
                        store,
                        conn_id,
                        provider,
                        upstream_model,
                        started,
                    )
                    .await;
                });
                return Ok(rx);
            }
        }
    }
    Err(fallback_error(&requested, &failures))
}

/// Non-streaming sibling: returns the full Anthropic message `Value`.
pub async fn anthropic_messages(ctx: &UpstreamCtx, body: Value) -> anyhow::Result<Value> {
    let requested = body["model"].as_str().unwrap_or("").to_string();
    let targets = route_models_for_anthropic_messages(&ctx.store, &requested).await?;
    if targets.is_empty() {
        anyhow::bail!("no enabled connection serves model '{requested}'");
    }

    let mut failures = Vec::new();
    for mut target in targets {
        if let Err(failure) = ensure_fresh_for_attempt(ctx, &mut target).await {
            let try_next = should_try_next_target(&failure);
            failures.push(failure);
            if try_next {
                continue;
            }
            return Err(fallback_error(&requested, &failures));
        }

        let mut attempt_body = body.clone();
        attempt_body["model"] = json!(target.upstream_model);
        attempt_body["stream"] = json!(false);
        match target.desc.format {
            ApiFormat::Anthropic => {
                let provider = target.conn.provider.clone();
                let tool_map = claude_cloak_map_for(&target, &attempt_body);
                let resp = send_upstream(ctx, &mut target, &attempt_body).await?;
                if !resp.status().is_success() {
                    let failure = upstream_status_failure(provider, resp).await;
                    let try_next = should_try_next_target(&failure);
                    failures.push(failure);
                    if try_next {
                        continue;
                    }
                    return Err(fallback_error(&requested, &failures));
                }
                let mut v: Value = resp.json().await?;
                claude_cloak::decloak_response(&mut v, &tool_map);
                return Ok(v);
            }
            ApiFormat::OpenAi => {
                let upstream_body = translate::openai_to_anthropic_request(&attempt_body)
                    .or_else(|_| translate::anthropic_to_openai_request(&attempt_body))?;
                let provider = target.conn.provider.clone();
                let resp = send_upstream(ctx, &mut target, &upstream_body).await?;
                if !resp.status().is_success() {
                    let failure = upstream_status_failure(provider, resp).await;
                    let try_next = should_try_next_target(&failure);
                    failures.push(failure);
                    if try_next {
                        continue;
                    }
                    return Err(fallback_error(&requested, &failures));
                }
                let v: Value = resp.json().await?;
                return Ok(translate::openai_to_anthropic_response(&v));
            }
        }
    }
    Err(fallback_error(&requested, &failures))
}

/// Pump an Anthropic-format upstream SSE response into `(name, Value)` events.
#[allow(clippy::too_many_arguments)]
async fn pump_anthropic(
    resp: reqwest::Response,
    tx: mpsc::Sender<anyhow::Result<AnthropicEvent>>,
    store: Arc<Store>,
    conn_id: String,
    provider: String,
    model: String,
    started: i64,
    tool_map: claude_cloak::ToolNameMap,
) {
    use crate::llm_router::sse::SseParser;
    use futures::StreamExt;
    let mut parser = SseParser::default();
    let mut stream = resp.bytes_stream();
    let mut input_tokens = 0i64;
    let mut output_tokens = 0i64;
    let mut errored = false;
    'pump: while let Some(item) = stream.next().await {
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                let _ = tx
                    .send(Err(anyhow::anyhow!("upstream stream interrupted: {e}")))
                    .await;
                errored = true;
                break;
            }
        };
        for ev in parser.feed(&chunk) {
            if ev.data == "[DONE]" {
                continue;
            }
            let Ok(mut v) = serde_json::from_str::<Value>(&ev.data) else {
                continue;
            };
            let name = ev.event.clone().unwrap_or_default();
            claude_cloak::decloak_event(&name, &mut v, &tool_map);
            if let Some(u) = v.get("message").and_then(|m| m.get("usage")) {
                input_tokens += u.get("input_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
            }
            if let Some(u) = v.get("usage") {
                output_tokens += u.get("output_tokens").and_then(|x| x.as_i64()).unwrap_or(0);
            }
            if tx.send(Ok((name, v))).await.is_err() {
                break 'pump; // consumer dropped
            }
        }
    }
    crate::llm_router::usage::record(
        &store,
        &conn_id,
        &provider,
        &model,
        "native",
        crate::llm_router::usage::Usage {
            input: input_tokens,
            output: output_tokens,
        },
        if errored { 502 } else { 200 },
        started,
        errored.then(|| "stream interrupted".to_string()),
    );
}

/// Pump an OpenAI-format upstream SSE response through the existing
/// OpenAI→Anthropic translator, emitting `(name, Value)` Anthropic events.
#[allow(clippy::too_many_arguments)]
async fn pump_openai_translated(
    resp: reqwest::Response,
    model: String,
    tx: mpsc::Sender<anyhow::Result<AnthropicEvent>>,
    store: Arc<Store>,
    conn_id: String,
    provider: String,
    upstream_model: String,
    started: i64,
) {
    use crate::llm_router::sse::SseParser;
    use futures::StreamExt;
    let mut parser = SseParser::default();
    let mut tr = translate::OpenAiToAnthropicStream::new(&model);
    let mut stream = resp.bytes_stream();
    let mut errored = false;
    'pump: while let Some(item) = stream.next().await {
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                for (name, data) in tr.error_frame(&format!("upstream stream interrupted: {e}")) {
                    let _ = tx.send(Ok((name, data))).await;
                }
                errored = true;
                break;
            }
        };
        for ev in parser.feed(&chunk) {
            if ev.data == "[DONE]" {
                continue;
            }
            if let Ok(v) = serde_json::from_str::<Value>(&ev.data) {
                for (name, data) in tr.feed(&v) {
                    if tx.send(Ok((name, data))).await.is_err() {
                        break 'pump; // consumer dropped
                    }
                }
            }
        }
    }
    if !errored {
        if tr.saw_terminal() {
            for (name, data) in tr.finish() {
                let _ = tx.send(Ok((name, data))).await;
            }
        } else {
            for (name, data) in tr.error_frame("upstream stream ended without a terminal event") {
                let _ = tx.send(Ok((name, data))).await;
            }
            errored = true;
        }
    }
    let (input, output) = tr.usage();
    crate::llm_router::usage::record(
        &store,
        &conn_id,
        &provider,
        &upstream_model,
        "native",
        crate::llm_router::usage::Usage { input, output },
        if errored { 502 } else { 200 },
        started,
        errored.then(|| "stream interrupted".to_string()),
    );
}

// ---------------------------------------------------------------------------
// Kiro (AWS CodeWhisperer) — in-process native support
//
// The endpoint server (`server.rs`) already serves kiro; this is the same
// pipeline retargeted from its `AppState` to `UpstreamCtx` so the native
// runtime can drive kiro too (Anthropic body → kiro `generateAssistantResponse`
// → AWS EventStream → OpenAI chunks → Anthropic events). The translators
// (`kiro`, `aws_stream`, `translate::OpenAiToAnthropicStream`) are shared.
// ---------------------------------------------------------------------------

/// Ordered kiro `generateAssistantResponse` endpoints. Account-bound auth
/// (api_key/idc/external_idp) puts the two `amazonaws.com` hosts first because
/// kiro.dev rejects an account-bound bearer token. Mirrors server.rs.
fn kiro_endpoints(auth_method: &str, region: &str) -> Vec<String> {
    let kiro_dev = "https://runtime.us-east-1.kiro.dev/generateAssistantResponse".to_string();
    let codewhisperer =
        format!("https://codewhisperer.{region}.amazonaws.com/generateAssistantResponse");
    let q = format!("https://q.{region}.amazonaws.com/generateAssistantResponse");
    if connections::is_account_bound(auth_method) {
        vec![codewhisperer, q, kiro_dev]
    } else {
        vec![kiro_dev, codewhisperer, q]
    }
}

/// Build the verbatim kiro upstream request (wire contract only — no cloaking).
fn kiro_upstream_request(
    ctx: &UpstreamCtx,
    target: &RouteTarget,
    kiro_body: &Value,
) -> reqwest::RequestBuilder {
    let data = &target.conn.data;
    let auth_method = connections::kiro_auth_method(data);
    let url = kiro_endpoints(&auth_method, &connections::kiro_region(data))
        .into_iter()
        .next()
        .expect("kiro_endpoints always returns at least one URL");
    let token = data.access_token.clone().unwrap_or_default();
    let mut req = ctx
        .http
        .post(url)
        .header("content-type", "application/json")
        .header("accept", "application/vnd.amazon.eventstream")
        .header(
            "x-amz-target",
            "AmazonCodeWhispererStreamingService.GenerateAssistantResponse",
        )
        .header("user-agent", "AWS-SDK-JS/3.0.0 kiro-ide/1.0.0")
        .header("x-amz-user-agent", "aws-sdk-js/3.0.0 kiro-ide/1.0.0")
        .header("amz-sdk-request", "attempt=1; max=3")
        .header("amz-sdk-invocation-id", uuid::Uuid::new_v4().to_string())
        .header("authorization", format!("Bearer {token}"));
    if auth_method == "api_key" {
        req = req.header("tokentype", "API_KEY");
    } else if auth_method == "external_idp" {
        req = req.header("TokenType", "EXTERNAL_IDP");
    }
    req.json(kiro_body)
}

/// Send the kiro request; on 401/403 refresh once and retry (mirrors
/// [`send_upstream`]). Pre-stream only — a mid-stream 401 is not retried.
async fn send_kiro(
    ctx: &UpstreamCtx,
    target: &mut RouteTarget,
    kiro_body: &Value,
) -> anyhow::Result<reqwest::Response> {
    let resp = kiro_upstream_request(ctx, target, kiro_body).send().await?;
    if matches!(resp.status().as_u16(), 401 | 403) {
        let refreshed = match &ctx.oauth_token_url_override {
            Some(token_url) => oauth::refresh::force_refresh_with_token_url(
                &ctx.store,
                &ctx.http,
                &mut target.conn,
                token_url,
            )
            .await
            .is_ok(),
            None => oauth::refresh::force_refresh(&ctx.store, &ctx.http, &mut target.conn)
                .await
                .is_ok(),
        };
        if refreshed {
            return Ok(kiro_upstream_request(ctx, target, kiro_body).send().await?);
        }
    }
    Ok(resp)
}

/// Start a kiro stream for the native path: translate the Anthropic body to
/// kiro's payload, send, and spawn a pump that yields Anthropic events. Returns
/// the receiver on a successful (2xx) upstream; a non-2xx becomes an
/// [`UpstreamAttemptFailure`] so the caller can try the next fallback target.
async fn kiro_stream(
    ctx: &UpstreamCtx,
    target: &mut RouteTarget,
    body: &Value,
    started: i64,
) -> Result<mpsc::Receiver<anyhow::Result<AnthropicEvent>>, UpstreamAttemptFailure> {
    let conversation_id = uuid::Uuid::new_v4().to_string();
    let kiro_body = crate::llm_router::kiro::anthropic_request_to_kiro(
        body,
        &target.upstream_model,
        &target.conn.data,
        &conversation_id,
    );
    let resp = send_kiro(ctx, target, &kiro_body)
        .await
        .map_err(|e| UpstreamAttemptFailure {
            provider: target.conn.provider.clone(),
            message: format!("upstream kiro: {e}"),
            status: None,
        })?;
    if !resp.status().is_success() {
        return Err(upstream_status_failure(target.conn.provider.clone(), resp).await);
    }
    let (tx, rx) = mpsc::channel::<anyhow::Result<AnthropicEvent>>(64);
    let store = ctx.store.clone();
    let conn_id = target.conn.id.clone();
    let provider = target.conn.provider.clone();
    let model = target.upstream_model.clone();
    tokio::spawn(async move {
        pump_kiro(resp, model, tx, store, conn_id, provider, started).await;
    });
    Ok(rx)
}

/// Pump a kiro AWS-EventStream response into Anthropic events: decode frames
/// with `AwsEventStreamParser`, turn them into OpenAI chunks via
/// `KiroToOpenAiStream`, then translate those into Anthropic events with the
/// same `OpenAiToAnthropicStream` the openrouter pump uses.
async fn pump_kiro(
    resp: reqwest::Response,
    model: String,
    tx: mpsc::Sender<anyhow::Result<AnthropicEvent>>,
    store: Arc<Store>,
    conn_id: String,
    provider: String,
    started: i64,
) {
    use futures::StreamExt;
    let mut parser = crate::llm_router::aws_stream::AwsEventStreamParser::default();
    let mut kiro = crate::llm_router::kiro::KiroToOpenAiStream::new(&model);
    let mut tr = translate::OpenAiToAnthropicStream::new(&model);
    let mut stream = resp.bytes_stream();
    let mut errored = false;
    'pump: while let Some(item) = stream.next().await {
        let chunk = match item {
            Ok(c) => c,
            Err(e) => {
                for (name, data) in tr.error_frame(&format!("upstream stream interrupted: {e}")) {
                    let _ = tx.send(Ok((name, data))).await;
                }
                errored = true;
                break;
            }
        };
        for frame in parser.feed(&chunk) {
            for oai in kiro.feed(&frame) {
                for (name, data) in tr.feed(&oai) {
                    if tx.send(Ok((name, data))).await.is_err() {
                        break 'pump; // consumer dropped
                    }
                }
            }
        }
    }
    if !errored {
        // A clean EOF is a valid completion even if no explicit terminal frame
        // (`messageStopEvent`/`metadataEvent`) was seen — Kiro ends some turns
        // that way (matches 9router, which finishes on EOF). Emit the finish
        // chunk via `finish_on_eof` and translate it, rather than erroring; a
        // genuine mid-stream break is a transport `Err` and is handled above.
        if !kiro.saw_terminal() {
            for oai in kiro.finish() {
                for (name, data) in tr.feed(&oai) {
                    let _ = tx.send(Ok((name, data))).await;
                }
            }
        }
        for (name, data) in tr.finish() {
            let _ = tx.send(Ok((name, data))).await;
        }
    }
    let (input, output) = kiro.usage();
    crate::llm_router::usage::record(
        &store,
        &conn_id,
        &provider,
        &model,
        "native",
        crate::llm_router::usage::Usage { input, output },
        if errored { 502 } else { 200 },
        started,
        errored.then(|| "stream interrupted".to_string()),
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm_router::connections::{ConnectionData, ConnectionRow};
    use crate::llm_router::models::CLAUDE_CODE_SYSTEM_PROMPT;

    fn mk_conn(id: &str, provider: &str, auth_type: &str, data: ConnectionData) -> ConnectionRow {
        ConnectionRow {
            id: id.into(),
            provider: provider.into(),
            auth_type: auth_type.into(),
            label: "t".into(),
            priority: 0,
            enabled: true,
            data,
            created_at: 0,
            updated_at: 0,
        }
    }

    async fn test_ctx() -> UpstreamCtx {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        UpstreamCtx::new(store)
    }

    #[test]
    fn inject_system_prompt_wraps_string_system() {
        let mut body = json!({"system": "be nice", "messages": []});
        inject_claude_system_prompt(&mut body);
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 2);
        assert_eq!(sys[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(sys[1]["text"], "be nice");
    }

    #[test]
    fn extract_upstream_error_reads_every_common_shape_and_falls_back_to_raw() {
        // Anthropic / OpenAI
        assert_eq!(
            extract_upstream_error_message(r#"{"error":{"message":"model not found"}}"#),
            "model not found"
        );
        // Bare {"error":"..."}
        assert_eq!(
            extract_upstream_error_message(r#"{"error":"invalid_grant"}"#),
            "invalid_grant"
        );
        // AWS / Kiro lowercase + capitalized message keys
        assert_eq!(
            extract_upstream_error_message(r#"{"message":"ValidationException: bad tool schema"}"#),
            "ValidationException: bad tool schema"
        );
        assert_eq!(
            extract_upstream_error_message(
                r#"{"__type":"ThrottlingException","Message":"Rate exceeded"}"#
            ),
            "Rate exceeded"
        );
        // Non-JSON body is surfaced verbatim (trimmed), NOT swallowed.
        assert_eq!(
            extract_upstream_error_message("  Bad Gateway  "),
            "Bad Gateway"
        );
        // Truly empty body → generic sentinel.
        assert_eq!(extract_upstream_error_message("   "), "upstream error");
    }

    #[test]
    fn inject_system_prompt_prepends_to_array_system() {
        let mut body = json!({"system": [{"type": "text", "text": "custom block"}]});
        inject_claude_system_prompt(&mut body);
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 2);
        assert_eq!(sys[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(sys[1]["text"], "custom block");
    }

    #[test]
    fn inject_system_prompt_is_idempotent_when_already_present() {
        let mut body = json!({"system": [
            {"type": "text", "text": CLAUDE_CODE_SYSTEM_PROMPT},
            {"type": "text", "text": "custom block"}
        ]});
        inject_claude_system_prompt(&mut body);
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 2, "must not duplicate an already-present prefix");
        assert_eq!(sys[1]["text"], "custom block");
    }

    #[test]
    fn inject_system_prompt_sets_when_absent() {
        let mut body = json!({"messages": []});
        inject_claude_system_prompt(&mut body);
        let sys = body["system"].as_array().unwrap();
        assert_eq!(sys.len(), 1);
        assert_eq!(sys[0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
    }

    #[tokio::test]
    async fn oauth_request_for_anthropic_uses_access_token_and_injects_system_prompt() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("anthropic-oauth").unwrap();
        let conn = mk_conn(
            "c1",
            "anthropic-oauth",
            "oauth",
            ConnectionData {
                api_key: Some("should-not-be-used".into()),
                access_token: Some("at-secret".into()),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "claude-x".into(),
        };
        let body = json!({"model": "claude-x", "system": "be helpful", "messages": []});
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://api.anthropic.com/v1/messages?beta=true"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer at-secret"
        );
        assert_eq!(
            req.headers().get("anthropic-beta").unwrap(),
            crate::llm_router::models::ANTHROPIC_OAUTH_BETA
        );
        assert_eq!(
            req.headers().get("anthropic-version").unwrap(),
            "2023-06-01"
        );
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(sent["system"][0]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(sent["system"][1]["text"], "be helpful");
    }

    #[tokio::test]
    async fn oauth_request_for_anthropic_applies_full_cloak_when_enabled() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("anthropic-oauth").unwrap();
        let conn = mk_conn(
            "c1",
            "anthropic-oauth",
            "oauth",
            ConnectionData {
                access_token: Some("sk-ant-oat-test".into()),
                provider_specific: Some(json!({"claudeCloaking": true})),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "claude-x".into(),
        };
        let body = json!({
            "model": "claude-x",
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "tu_1", "name": "lookup", "input": {"q": "x"}}
                ]}
            ],
            "tools": [{"name": "lookup", "description": "Lookup data", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "tool", "name": "lookup"}
        });
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();

        assert!(req.headers().contains_key("x-stainless-runtime-version"));
        assert!(req.headers().contains_key("x-claude-code-session-id"));
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert!(sent["system"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("x-anthropic-billing-header: cc_version=2.1.92."));
        assert_eq!(sent["system"][1]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(sent["tools"][0]["name"], "lookup_ide");
        assert!(sent["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "Bash"));
        assert_eq!(sent["messages"][0]["content"][0]["name"], "lookup_ide");
        assert_eq!(sent["tool_choice"]["name"], "lookup_ide");
        assert!(sent["metadata"]["user_id"]
            .as_str()
            .unwrap()
            .contains("\"session_id\""));
    }

    #[tokio::test]
    async fn oauth_request_for_openai_hits_codex_responses_with_account_and_session_headers() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("openai-oauth").unwrap();
        let conn = mk_conn(
            "c2",
            "openai-oauth",
            "oauth",
            ConnectionData {
                access_token: Some("at-codex".into()),
                provider_specific: Some(json!({"chatgpt_account_id": "acct-1"})),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "gpt-5.2-codex".into(),
        };
        let body = json!({"model": "gpt-5.2-codex", "input": "hi"});
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://chatgpt.com/backend-api/codex/responses"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer at-codex"
        );
        assert_eq!(req.headers().get("originator").unwrap(), "codex_cli_rs");
        assert_eq!(req.headers().get("chatgpt-account-id").unwrap(), "acct-1");
        assert!(req.headers().get("session_id").is_some());
    }

    #[tokio::test]
    async fn oauth_request_for_openai_omits_account_header_when_absent() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("openai-oauth").unwrap();
        let conn = mk_conn(
            "c3",
            "openai-oauth",
            "oauth",
            ConnectionData {
                access_token: Some("at-codex".into()),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "gpt-5.2-codex".into(),
        };
        let body = json!({"model": "gpt-5.2-codex", "input": "hi"});
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert!(req.headers().get("chatgpt-account-id").is_none());
    }

    #[tokio::test]
    async fn anthropic_messages_route_skips_responses_only_route_target() {
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "chatgpt",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("at-codex".into()),
                    models_override: Some(vec!["gpt-5.2-codex".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "claude",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-ant".into()),
                    models_override: Some(vec!["claude-sonnet-4-5".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r1".into(),
                name: "fable".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![
                    routes::ModelRouteTarget {
                        connection_id: "chatgpt".into(),
                        model: "gpt-5.2-codex".into(),
                    },
                    routes::ModelRouteTarget {
                        connection_id: "claude".into(),
                        model: "claude-sonnet-4-5".into(),
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let target = route_model_for_anthropic_messages(&ctx.store, "fable")
            .await
            .unwrap()
            .unwrap();

        assert_eq!(target.conn.id, "claude");
        assert_eq!(target.conn.provider, "anthropic");
        assert_eq!(target.upstream_model, "claude-sonnet-4-5");
    }

    #[tokio::test]
    async fn selectable_native_models_lists_usable_routes_then_connection_models_and_skips_unreachable(
    ) {
        let ctx = test_ctx().await;
        // Codex: Responses-only — excluded from the native path entirely.
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "chatgpt",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("at-codex".into()),
                    models_override: Some(vec!["gpt-5.2-codex".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        // OpenRouter WITH a key — drivable in-process (OpenAI-format + translation).
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "or",
                "openrouter",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-or".into()),
                    models_override: Some(vec!["deepseek/deepseek-chat:free".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        // OpenRouter WITHOUT a key — must NOT be offered (would error on send).
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "or-nokey",
                "openrouter",
                "api_key",
                ConnectionData {
                    api_key: None,
                    models_override: Some(vec!["keyless/model".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        // Kiro (oauth) IS drivable natively now (AWS EventStream pipeline), so
        // its models must be offered.
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "kiro1",
                "kiro",
                "oauth",
                ConnectionData {
                    access_token: Some("at-kiro".into()),
                    models_override: Some(vec!["claude-sonnet-5".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        // A usable route (openrouter target with a key) and a Codex-only route
        // that must be filtered out because native can't drive Codex.
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r1".into(),
                name: "usable-combo".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![routes::ModelRouteTarget {
                    connection_id: "or".into(),
                    model: "deepseek/deepseek-chat:free".into(),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r2".into(),
                name: "codex-only".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![routes::ModelRouteTarget {
                    connection_id: "chatgpt".into(),
                    model: "gpt-5.2-codex".into(),
                }],
                created_at: 2,
                updated_at: 2,
            },
        )
        .await
        .unwrap();

        let models = selectable_native_models(&ctx.store).await;

        // Usable route first; the openrouter provider/model is offered.
        assert_eq!(models.first().map(String::as_str), Some("usable-combo"));
        assert!(models
            .iter()
            .any(|m| m == "openrouter/deepseek/deepseek-chat:free"));
        // Kiro is drivable natively — its models are offered.
        assert!(
            models.iter().any(|m| m == "kiro/claude-sonnet-5"),
            "kiro must be offered, got: {models:?}"
        );
        // Codex is Responses-only — neither its model nor a Codex-only route.
        assert!(
            !models.iter().any(|m| m.starts_with("openai-oauth/")),
            "got: {models:?}"
        );
        assert!(
            !models.iter().any(|m| m == "codex-only"),
            "codex-only route must be filtered, got: {models:?}"
        );
        // A keyless connection's models are never offered.
        assert!(
            !models.iter().any(|m| m == "openrouter/keyless/model"),
            "got: {models:?}"
        );
    }

    #[tokio::test]
    async fn model_route_expands_same_provider_account_fallbacks() {
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "other-account",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-other".into()),
                    models_override: Some(vec!["claude-fable-5".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "picked-account",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-picked".into()),
                    models_override: Some(vec!["claude-fable-5".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r1".into(),
                name: "task".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![routes::ModelRouteTarget {
                    connection_id: "picked-account".into(),
                    model: "claude-fable-5".into(),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let targets = route_models_for_anthropic_messages(&ctx.store, "task")
            .await
            .unwrap();
        let ids = targets
            .iter()
            .map(|target| target.conn.id.as_str())
            .collect::<Vec<_>>();

        assert_eq!(ids, vec!["picked-account", "other-account"]);
        assert!(targets
            .iter()
            .all(|target| target.upstream_model == "claude-fable-5"));
    }

    #[tokio::test]
    async fn model_route_skips_api_key_targets_without_credentials() {
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "openrouter-missing-key",
                "openrouter",
                "api_key",
                ConnectionData {
                    models_override: Some(vec!["z-ai/glm-5.2".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "claude",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-ant".into()),
                    models_override: Some(vec!["claude-sonnet-4-5".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r1".into(),
                name: "task".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![
                    routes::ModelRouteTarget {
                        connection_id: "openrouter-missing-key".into(),
                        model: "z-ai/glm-5.2".into(),
                    },
                    routes::ModelRouteTarget {
                        connection_id: "claude".into(),
                        model: "claude-sonnet-4-5".into(),
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let targets = route_models_for_anthropic_messages(&ctx.store, "task")
            .await
            .unwrap();
        let providers = targets
            .iter()
            .map(|target| target.conn.provider.as_str())
            .collect::<Vec<_>>();

        assert_eq!(providers, vec!["anthropic"]);
    }

    #[tokio::test]
    async fn default_anthropic_messages_model_prefers_compatible_named_route() {
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "chatgpt",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("at-codex".into()),
                    models_override: Some(vec!["gpt-5.2-codex".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "claude",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-ant".into()),
                    models_override: Some(vec!["claude-sonnet-4-5".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r1".into(),
                name: "fable".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![routes::ModelRouteTarget {
                    connection_id: "claude".into(),
                    model: "claude-sonnet-4-5".into(),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            default_anthropic_messages_model(&ctx.store)
                .await
                .as_deref(),
            Some("fable")
        );
    }

    #[tokio::test]
    async fn anthropic_messages_falls_back_when_first_route_target_hits_quota() {
        use axum::{routing::post, Json, Router};

        async fn quota() -> (axum::http::StatusCode, Json<Value>) {
            (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": {"message": "You're out of extra usage."}})),
            )
        }

        async fn ok(Json(body): Json<Value>) -> Json<Value> {
            Json(json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": body["model"].clone(),
                "content": [{"type": "text", "text": "fallback worked"}],
                "usage": {"input_tokens": 1, "output_tokens": 2},
            }))
        }

        let app = Router::new()
            .route("/first/messages", post(quota))
            .route("/second/messages", post(ok));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "first",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-first".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}/first")),
                    models_override: Some(vec!["claude-first".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "second",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-second".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}/second")),
                    models_override: Some(vec!["claude-second".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r1".into(),
                name: "fable".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![
                    routes::ModelRouteTarget {
                        connection_id: "first".into(),
                        model: "claude-first".into(),
                    },
                    routes::ModelRouteTarget {
                        connection_id: "second".into(),
                        model: "claude-second".into(),
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let response = anthropic_messages(
            &ctx,
            json!({
                "model": "fable",
                "messages": [{"role": "user", "content": "hi"}],
            }),
        )
        .await
        .unwrap();

        assert_eq!(response["model"], "claude-second");
        assert_eq!(response["content"][0]["text"], "fallback worked");
    }

    #[tokio::test]
    async fn anthropic_messages_decloaks_tool_names_for_cloaked_anthropic_oauth() {
        use axum::{routing::post, Json, Router};

        async fn ok(Json(body): Json<Value>) -> Json<Value> {
            assert_eq!(body["tools"][0]["name"], "lookup_ide");
            Json(json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": body["model"].clone(),
                "content": [{"type": "tool_use", "id": "tu_1", "name": "lookup_ide", "input": {"q": "x"}}],
                "stop_reason": "tool_use",
                "usage": {"input_tokens": 1, "output_tokens": 2},
            }))
        }

        let app = Router::new().route("/messages", post(ok));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        let now = crate::paths::now_ms();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "claude-oauth",
                "anthropic-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("sk-ant-oat-test".into()),
                    expires_at: Some(now + 24 * 60 * 60 * 1000),
                    last_refresh_at: Some(now),
                    base_url_override: Some(format!("http://127.0.0.1:{port}")),
                    models_override: Some(vec!["claude-x".into()]),
                    provider_specific: Some(json!({"claudeCloaking": true})),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let response = anthropic_messages(
            &ctx,
            json!({
                "model": "anthropic-oauth/claude-x",
                "messages": [{"role": "user", "content": "hi"}],
                "tools": [{"name": "lookup", "description": "Lookup data", "input_schema": {"type": "object"}}]
            }),
        )
        .await
        .unwrap();

        assert_eq!(response["content"][0]["name"], "lookup");
    }

    #[tokio::test]
    async fn anthropic_messages_falls_back_to_second_account_for_same_route_target() {
        use axum::{routing::post, Json, Router};

        async fn quota() -> (axum::http::StatusCode, Json<Value>) {
            (
                axum::http::StatusCode::TOO_MANY_REQUESTS,
                Json(json!({"error": {"message": "You're out of extra usage."}})),
            )
        }

        async fn ok(Json(body): Json<Value>) -> Json<Value> {
            Json(json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": body["model"].clone(),
                "content": [{"type": "text", "text": "second account worked"}],
                "usage": {"input_tokens": 1, "output_tokens": 2},
            }))
        }

        let app = Router::new()
            .route("/primary/messages", post(quota))
            .route("/secondary/messages", post(ok));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "secondary",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-secondary".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}/secondary")),
                    models_override: Some(vec!["claude-fable-5".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "primary",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-primary".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}/primary")),
                    models_override: Some(vec!["claude-fable-5".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r1".into(),
                name: "task".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![routes::ModelRouteTarget {
                    connection_id: "primary".into(),
                    model: "claude-fable-5".into(),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let response = anthropic_messages(
            &ctx,
            json!({
                "model": "task",
                "messages": [{"role": "user", "content": "hi"}],
            }),
        )
        .await
        .unwrap();

        assert_eq!(response["model"], "claude-fable-5");
        assert_eq!(response["content"][0]["text"], "second account worked");
    }

    #[tokio::test]
    async fn free_provider_uses_public_bearer_and_opencode_client_header() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("opencode-free").unwrap();
        let conn = mk_conn("c4", "opencode-free", "none", ConnectionData::default());
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "grok-code".into(),
        };
        let body = json!({"model": "grok-code", "messages": []});
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://opencode.ai/zen/v1/chat/completions"
        );
        assert_eq!(req.headers().get("authorization").unwrap(), "Bearer public");
        assert_eq!(req.headers().get("x-opencode-client").unwrap(), "desktop");
    }

    #[tokio::test]
    async fn api_key_provider_is_unaffected_by_oauth_free_branches() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("openai").unwrap();
        let conn = mk_conn(
            "c5",
            "openai",
            "api_key",
            ConnectionData {
                api_key: Some("sk-live".into()),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "gpt-5.2".into(),
        };
        let body = json!({"model": "gpt-5.2", "messages": []});
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://api.openai.com/v1/chat/completions"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer sk-live"
        );
    }

    #[test]
    fn message_stream_event_decodes_text_delta() {
        let ev = (
            "content_block_delta".to_string(),
            json!({"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"hi"}}),
        );
        assert_eq!(
            MessageStreamEvent::from_event(&ev),
            Some(MessageStreamEvent::TextDelta {
                index: 0,
                text: "hi".into()
            })
        );
    }

    #[test]
    fn message_stream_event_decodes_tool_use_start_and_input_delta() {
        let start = (
            "content_block_start".to_string(),
            json!({"type":"content_block_start","index":1,"content_block":{"type":"tool_use","id":"t1","name":"bash","input":{}}}),
        );
        match MessageStreamEvent::from_event(&start).unwrap() {
            MessageStreamEvent::ContentBlockStart { index, block } => {
                assert_eq!(index, 1);
                assert_eq!(block["name"], "bash");
                assert_eq!(block["id"], "t1");
            }
            other => panic!("expected ContentBlockStart, got {other:?}"),
        }
        let delta = (
            "content_block_delta".to_string(),
            json!({"type":"content_block_delta","index":1,"delta":{"type":"input_json_delta","partial_json":"{\"cmd\":"}}),
        );
        assert_eq!(
            MessageStreamEvent::from_event(&delta),
            Some(MessageStreamEvent::InputJsonDelta {
                index: 1,
                partial_json: "{\"cmd\":".into()
            })
        );
    }

    #[test]
    fn message_stream_event_decodes_stop_and_delta() {
        let md = (
            "message_delta".to_string(),
            json!({"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":7}}),
        );
        assert_eq!(
            MessageStreamEvent::from_event(&md),
            Some(MessageStreamEvent::MessageDelta {
                stop_reason: Some("tool_use".into()),
                output_tokens: 7
            })
        );
        let stop = ("message_stop".to_string(), json!({"type":"message_stop"}));
        assert_eq!(
            MessageStreamEvent::from_event(&stop),
            Some(MessageStreamEvent::MessageStop)
        );
    }
}
