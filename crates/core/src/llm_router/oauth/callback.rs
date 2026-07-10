//! Loopback OAuth callback server + end-to-end flow orchestration: PKCE +
//! authorize URL + browser-open + callback capture + token exchange +
//! persisting a new OAuth connection. Also a manual (paste) fallback for
//! environments where a browser can't reach the loopback listener.
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors).
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use tokio::net::TcpListener;

use crate::llm_router::connections::{self, ConnectionData, ConnectionRow};
use crate::llm_router::oauth::{flow, pkce, OAuthTokens};
use crate::llm_router::registry::{oauth_config, RedirectMode};
use crate::oauth_loopback::{await_callback, bind_fixed, bind_random, spawn_callback_server};
use crate::store::Store;

fn callback_path(mode: RedirectMode) -> &'static str {
    match mode {
        RedirectMode::LoopbackRandom => "/callback",
        RedirectMode::LoopbackFixed(_) => "/auth/callback",
    }
}

fn redirect_uri_for(mode: RedirectMode, bound_port: u16) -> String {
    match mode {
        RedirectMode::LoopbackRandom => format!("http://localhost:{bound_port}/callback"),
        RedirectMode::LoopbackFixed(p) => format!("http://localhost:{p}/auth/callback"),
    }
}

/// Bind the loopback listener for `mode`. `LoopbackFixed` bind failures are
/// mapped (inside `oauth_loopback::bind_fixed`) to an actionable message —
/// the fixed port is Codex's redirect requirement, so the only way it's
/// taken is another login already running.
async fn bind_loopback(mode: RedirectMode) -> Result<TcpListener> {
    match mode {
        RedirectMode::LoopbackRandom => bind_random().await,
        RedirectMode::LoopbackFixed(p) => bind_fixed(p).await,
    }
}

fn build_connection_row(provider: &str, label: &str, tokens: OAuthTokens) -> ConnectionRow {
    let now = crate::paths::now_ms();
    ConnectionRow {
        id: crate::paths::new_id(),
        provider: provider.to_string(),
        auth_type: "oauth".to_string(),
        label: label.to_string(),
        priority: 0,
        enabled: true,
        data: ConnectionData {
            access_token: Some(tokens.access_token),
            refresh_token: tokens.refresh_token,
            expires_at: Some(tokens.expires_at),
            provider_specific: tokens.provider_specific,
            ..Default::default()
        },
        created_at: now,
        updated_at: now,
    }
}

/// Apply freshly-exchanged `tokens` onto an `existing` connection row for a
/// reconnect: only the credential fields + `needs_relogin` + `updated_at`
/// change — `id`/`provider`/`auth_type`/`label`/`priority`/`enabled` are
/// preserved so the row keeps its identity and place in the routing order.
fn apply_reconnect_tokens(mut existing: ConnectionRow, tokens: OAuthTokens) -> ConnectionRow {
    existing.data.access_token = Some(tokens.access_token);
    existing.data.refresh_token = tokens.refresh_token;
    existing.data.expires_at = Some(tokens.expires_at);
    existing.data.provider_specific = tokens.provider_specific;
    existing.data.needs_relogin = Some(false);
    existing.updated_at = crate::paths::now_ms();
    existing
}

/// Persist the token-exchange result: inserts a new connection when
/// `existing_id` is `None` (the normal Add flow), or updates that connection
/// in place when `Some` (a Reconnect) so a stale `needs_relogin` row is never
/// left behind to shadow the fresh one in `route_model`'s priority order.
async fn persist_tokens(
    store: &Arc<Store>,
    provider: &str,
    label: &str,
    existing_id: Option<&str>,
    tokens: OAuthTokens,
) -> Result<ConnectionRow> {
    match existing_id {
        Some(id) => {
            let existing = connections::get_connection(store, id)
                .await?
                .with_context(|| format!("connection `{id}` not found for reconnect"))?;
            let row = apply_reconnect_tokens(existing, tokens);
            connections::update_connection(store, row.clone()).await?;
            Ok(row)
        }
        None => {
            let row = build_connection_row(provider, label, tokens);
            connections::add_connection(store, row.clone()).await?;
            Ok(row)
        }
    }
}

/// Run the full interactive OAuth flow against the provider's real,
/// registered token endpoint. `existing_id` is `None` for a normal Add (a
/// new connection is inserted) or `Some(id)` for a Reconnect (that
/// connection's tokens are updated in place — see [`persist_tokens`]).
pub async fn run_flow<F>(
    store: &Arc<Store>,
    http: &reqwest::Client,
    provider: &str,
    label: &str,
    existing_id: Option<&str>,
    open_browser: F,
    timeout: Duration,
) -> Result<ConnectionRow>
where
    F: FnOnce(&str),
{
    let cfg = oauth_config(provider)
        .with_context(|| format!("no OAuth config for provider `{provider}`"))?;
    run_flow_with_token_url(
        store,
        http,
        provider,
        label,
        cfg.token_url,
        existing_id,
        open_browser,
        timeout,
    )
    .await
}

/// Same as [`run_flow`] but against an explicit `token_url` — the seam tests
/// use to point the exchange at a mock server.
#[allow(clippy::too_many_arguments)]
pub async fn run_flow_with_token_url<F>(
    store: &Arc<Store>,
    http: &reqwest::Client,
    provider: &str,
    label: &str,
    token_url: &str,
    existing_id: Option<&str>,
    open_browser: F,
    timeout: Duration,
) -> Result<ConnectionRow>
where
    F: FnOnce(&str),
{
    let cfg = oauth_config(provider)
        .with_context(|| format!("no OAuth config for provider `{provider}`"))?;
    let pkce = pkce::generate();

    let listener = bind_loopback(cfg.redirect).await?;
    let bound_port = listener.local_addr()?.port();
    let path = callback_path(cfg.redirect);
    let redirect_uri = redirect_uri_for(cfg.redirect, bound_port);

    let authorize_url = flow::authorize_url(provider, &pkce, &redirect_uri)?;

    // Spawn the callback server before we hand the URL to the caller so the
    // accept loop is already running when `open_browser` fires.
    let (server, result_rx, shutdown_tx) = spawn_callback_server(listener, path);
    open_browser(&authorize_url);
    let callback = await_callback(server, result_rx, shutdown_tx, timeout).await?;

    let code = callback
        .code
        .context("OAuth callback did not include a `code` parameter")?;
    let state = callback
        .state
        .context("OAuth callback did not include a `state` parameter")?;
    if state != pkce.state {
        bail!("OAuth state mismatch — the authorization response did not match this request");
    }

    let tokens = flow::exchange_code_at(
        http,
        provider,
        token_url,
        &code,
        &state,
        &redirect_uri,
        &pkce.verifier,
    )
    .await?;

    persist_tokens(store, provider, label, existing_id, tokens).await
}

/// State handed back to the caller after starting the manual (paste)
/// fallback — no server is bound; the caller shows `authorize_url` to the
/// user and later feeds what they paste back into [`complete_manual`].
pub struct ManualStart {
    pub authorize_url: String,
    pub verifier: String,
    pub state: String,
    pub redirect_uri: String,
}

/// Reserve a loopback redirect_uri for the manual flow without leaving
/// anything listening. For `LoopbackRandom` we bind an ephemeral port
/// synchronously just long enough to read it back, then release it — the
/// provider never actually dereferences this URL (Anthropic's `code=true`
/// echoes the code+state directly on the page instead of redirecting), it
/// only needs to match a redirect_uri the client is registered for.
/// `LoopbackFixed` providers (Codex) must use the browser flow because the
/// provider redirects to the fixed callback URL instead of showing a code.
fn manual_redirect_uri(mode: RedirectMode) -> Result<String> {
    match mode {
        RedirectMode::LoopbackRandom => {
            let listener = std::net::TcpListener::bind("127.0.0.1:0")
                .context("failed to reserve a loopback port for the manual redirect_uri")?;
            let port = listener.local_addr()?.port();
            drop(listener);
            Ok(redirect_uri_for(mode, port))
        }
        RedirectMode::LoopbackFixed(_) => bail!(
            "manual paste OAuth is not available for fixed-port providers; use Connect with browser so Ryuzi can receive the callback"
        ),
    }
}

/// Begin the manual (paste) OAuth fallback: builds PKCE + the authorize URL,
/// but binds no server. The caller shows `authorize_url` to the user, who
/// pastes back the `code#state` (Anthropic) or bare `code` the provider
/// displays, which goes to [`complete_manual`].
pub fn begin_manual(provider: &str) -> Result<ManualStart> {
    let cfg = oauth_config(provider)
        .with_context(|| format!("no OAuth config for provider `{provider}`"))?;
    let pkce = pkce::generate();
    let redirect_uri = manual_redirect_uri(cfg.redirect)?;
    let authorize_url = flow::authorize_url(provider, &pkce, &redirect_uri)?;
    Ok(ManualStart {
        authorize_url,
        verifier: pkce.verifier,
        state: pkce.state,
        redirect_uri,
    })
}

/// Complete the manual (paste) OAuth fallback: splits the pasted
/// `code#state` (Anthropic) or bare code, verifies any embedded state
/// against the one [`begin_manual`] generated, exchanges the code, and
/// persists the resulting connection. `existing_id` behaves as in
/// [`run_flow`] — `Some(id)` updates that connection in place instead of
/// inserting a new one.
#[allow(clippy::too_many_arguments)]
pub async fn complete_manual(
    store: &Arc<Store>,
    http: &reqwest::Client,
    provider: &str,
    label: &str,
    existing_id: Option<&str>,
    verifier: &str,
    state: &str,
    pasted: &str,
    redirect_uri: &str,
) -> Result<ConnectionRow> {
    let cfg = oauth_config(provider)
        .with_context(|| format!("no OAuth config for provider `{provider}`"))?;
    let (code, parsed_state) = flow::split_manual_code(pasted);
    if let Some(ps) = &parsed_state {
        if ps != state {
            bail!("OAuth state mismatch — the pasted code does not match this request");
        }
    }
    let effective_state = parsed_state.unwrap_or_else(|| state.to_string());

    let tokens = flow::exchange_code_at(
        http,
        provider,
        cfg.token_url,
        &code,
        &effective_state,
        redirect_uri,
        verifier,
    )
    .await?;

    persist_tokens(store, provider, label, existing_id, tokens).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn callback_path_and_redirect_uri_match_provider_conventions() {
        assert_eq!(callback_path(RedirectMode::LoopbackRandom), "/callback");
        assert_eq!(
            callback_path(RedirectMode::LoopbackFixed(1455)),
            "/auth/callback"
        );
        assert_eq!(
            redirect_uri_for(RedirectMode::LoopbackRandom, 54321),
            "http://localhost:54321/callback"
        );
        assert_eq!(
            redirect_uri_for(RedirectMode::LoopbackFixed(1455), 0),
            "http://localhost:1455/auth/callback"
        );
    }

    #[tokio::test]
    async fn fixed_port_bind_failure_is_actionable() {
        // Hold the fixed port open ourselves, then try to bind it again the
        // same way run_flow would — the error should name the port and
        // explain what to do, not leak a raw OS error.
        let held = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
        let port = held.local_addr().unwrap().port();
        let err = bind_loopback(RedirectMode::LoopbackFixed(port))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains(&port.to_string()), "{msg}");
        assert!(msg.contains("already in use"), "{msg}");
    }

    #[test]
    fn manual_flow_rejects_fixed_port_providers() {
        let err = match begin_manual("openai-oauth") {
            Ok(_) => panic!("fixed-port OAuth should not start manual paste flow"),
            Err(err) => err,
        };
        let msg = err.to_string();
        assert!(msg.contains("manual paste"), "{msg}");
        assert!(msg.contains("Connect with browser"), "{msg}");
    }
}
