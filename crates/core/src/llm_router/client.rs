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

pub use crate::llm_router::provenance::AnthropicEvent;
use crate::llm_router::provenance::{
    classify_failure, RouteFailureCategory, RouteSelection, RouteSelectionReason, RoutedStream,
};
use crate::llm_router::registry::{self, ApiFormat, AuthScheme, ProviderDescriptor};
use crate::llm_router::{
    capabilities, claude_cloak, connections, mimo, model_capabilities, model_effort, oauth, routes,
    translate,
};
use crate::store::Store;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::sync::mpsc;

#[cfg(test)]
use crate::harness::native::capabilities::CapabilityResolutionError;
use crate::harness::native::capabilities::TransportToolCapabilities;

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
    /// Test-only override for Kiro's hard-coded `generateAssistantResponse`
    /// endpoint. `None` uses the production endpoint selection.
    pub kiro_base_override: Option<String>,
    /// Test-only override for the MiMo free-tier bootstrap endpoint that
    /// mints the anti-abuse JWT. `None` in production
    /// ([`mimo::BOOTSTRAP_URL`]).
    pub mimo_bootstrap_url_override: Option<String>,
}

impl UpstreamCtx {
    /// Construct a production context (no endpoint overrides).
    pub fn new(store: Arc<Store>) -> Self {
        Self {
            store,
            http: reqwest::Client::new(),
            oauth_token_url_override: None,
            kiro_base_override: None,
            mimo_bootstrap_url_override: None,
        }
    }
}

// ---------------------------------------------------------------------------
// Model routing (moved from server.rs — behavior unchanged)
// ---------------------------------------------------------------------------

#[derive(Clone)]
pub struct RouteTarget {
    pub conn: connections::ConnectionRow,
    pub desc: &'static ProviderDescriptor,
    pub upstream_model: String,
    pub route_target_key: Option<crate::llm_router::model_effort::RouteTargetEffortKey>,
}

#[derive(Clone)]
struct AnnotatedRouteTarget {
    target: RouteTarget,
    reason: RouteSelectionReason,
}

type ProviderOrderCache =
    std::collections::HashMap<(String, String, Vec<String>), (Vec<String>, RouteSelectionReason)>;

#[derive(Clone, Copy)]
enum RouteOrderMode {
    Advance,
    Peek,
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
    Ok(
        route_models_for_body_matching(store, requested, body, |_, _| true)
            .await?
            .into_iter()
            .map(|annotated| annotated.target)
            .collect(),
    )
}

pub async fn route_models_for_anthropic_messages(
    store: &Store,
    requested: &str,
) -> anyhow::Result<Vec<RouteTarget>> {
    Ok(
        route_models_for_body_matching(store, requested, None, anthropic_messages_target_allowed)
            .await?
            .into_iter()
            .map(|annotated| annotated.target)
            .collect(),
    )
}

/// Read the adapter-level tool envelope for every target the current routing
/// rules may try before content is delivered. This is deliberately a peek:
/// it never advances model-route or provider-account round-robin cursors.
pub async fn route_tool_capabilities(
    store: &Store,
    requested: &str,
) -> anyhow::Result<TransportToolCapabilities> {
    let mut provider_order_cache = ProviderOrderCache::new();
    let initial = route_models_for_body_matching_with_cache(
        store,
        requested,
        None,
        anthropic_messages_target_allowed,
        &mut provider_order_cache,
        RouteOrderMode::Peek,
    )
    .await?;
    if initial.is_empty() {
        return TransportToolCapabilities::intersection(std::iter::empty())
            .map_err(anyhow::Error::from);
    }
    let attempted = initial
        .iter()
        .map(|annotated| {
            (
                annotated.target.conn.id.clone(),
                annotated.target.upstream_model.clone(),
            )
        })
        .collect();
    let mut targets = initial;
    targets.extend(
        route_continuation_targets(
            store,
            requested,
            &attempted,
            &mut provider_order_cache,
            capabilities::ToolTransportRequirements::default(),
            RouteOrderMode::Peek,
        )
        .await?,
    );
    TransportToolCapabilities::intersection(
        targets
            .into_iter()
            .map(|annotated| target_tool_capabilities(&annotated.target)),
    )
    .map_err(anyhow::Error::from)
}

async fn route_models_for_body_matching(
    store: &Store,
    requested: &str,
    body: Option<&Value>,
    target_allowed: fn(&connections::ConnectionRow, &ProviderDescriptor) -> bool,
) -> anyhow::Result<Vec<AnnotatedRouteTarget>> {
    let mut provider_order_cache = ProviderOrderCache::new();
    route_models_for_body_matching_with_cache(
        store,
        requested,
        body,
        target_allowed,
        &mut provider_order_cache,
        RouteOrderMode::Advance,
    )
    .await
}

async fn route_models_for_body_matching_with_cache(
    store: &Store,
    requested: &str,
    body: Option<&Value>,
    target_allowed: fn(&connections::ConnectionRow, &ProviderDescriptor) -> bool,
    provider_order_cache: &mut ProviderOrderCache,
    order_mode: RouteOrderMode,
) -> anyhow::Result<Vec<AnnotatedRouteTarget>> {
    let conns = connections::list_connections(store).await?;
    let enabled: Vec<_> = conns.into_iter().filter(|c| c.enabled).collect();
    let required = body
        .map(capabilities::required_capabilities_from_body)
        .unwrap_or_default();
    let tool_requirements = body
        .map(capabilities::tool_transport_requirements_from_body)
        .unwrap_or_default();
    let route_list = routes::list_model_routes(store).await?;
    if let Some(route) = routes::route_by_name(&route_list, requested) {
        let indexed_targets = match order_mode {
            RouteOrderMode::Advance => routes::ordered_indexed_targets(store, route).await?,
            RouteOrderMode::Peek => route
                .targets
                .iter()
                .cloned()
                .enumerate()
                .map(|(index, target)| routes::IndexedModelRouteTarget {
                    original_index: index as u32,
                    target,
                })
                .collect(),
        };
        let targets = prefer_capable_indexed_targets(indexed_targets, required);
        let mut out = Vec::new();
        let route_reason = if route.targets.len() <= 1 {
            RouteSelectionReason::Initial
        } else if route.strategy == routes::ModelRouteStrategy::RoundRobin {
            RouteSelectionReason::RoundRobin
        } else {
            RouteSelectionReason::Ordered
        };
        let mut seen = std::collections::HashSet::<(String, String)>::new();
        for indexed in targets {
            for mut annotated in expanded_route_targets(
                store,
                &enabled,
                &indexed.target,
                target_allowed,
                route_reason.clone(),
                provider_order_cache,
                order_mode,
            )
            .await?
            {
                let key = (
                    annotated.target.conn.id.clone(),
                    annotated.target.upstream_model.clone(),
                );
                if !seen.insert(key) {
                    continue;
                }
                annotated.target.route_target_key =
                    Some(crate::llm_router::model_effort::RouteTargetEffortKey {
                        route_id: route.id.clone(),
                        target_index: indexed.original_index,
                    });
                out.push(annotated);
            }
        }
        return Ok(filter_tool_compatible(
            normalize_single_reason(out),
            tool_requirements,
        ));
    }
    if let Some((prov, model)) = requested.split_once('/') {
        let candidates: Vec<_> = enabled
            .into_iter()
            .filter(|conn| {
                registry::descriptor(&conn.provider)
                    .map(|desc| {
                        desc.family == prov
                            && target_allowed(conn, desc)
                            && connection_has_required_credentials(desc, conn)
                            && resolved_requested_model(conn, desc, requested, model, true)
                                .is_some()
                    })
                    .unwrap_or(false)
            })
            .collect();
        let mut out = Vec::new();
        for (conn, reason) in ordered_provider_connections(
            store,
            prov,
            model,
            candidates,
            provider_order_cache,
            order_mode,
        )
        .await?
        {
            if let Some(desc) = registry::descriptor(&conn.provider) {
                let Some(upstream_model) =
                    resolved_requested_model(&conn, desc, requested, model, true)
                else {
                    continue;
                };
                out.push(AnnotatedRouteTarget {
                    target: RouteTarget {
                        conn,
                        desc,
                        upstream_model,
                        route_target_key: None,
                    },
                    reason,
                });
            }
        }
        return Ok(filter_tool_compatible(
            normalize_single_reason(out),
            tool_requirements,
        ));
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
            let family = desc.family.to_string();
            if !grouped.contains_key(&family) {
                provider_order.push(family.clone());
            }
            grouped.entry(family).or_default().push(conn);
        }
    }
    let mut out = Vec::new();
    let cross_provider_ordered = provider_order.len() > 1;
    for provider in provider_order {
        let candidates = grouped.remove(&provider).unwrap_or_default();
        for (conn, account_reason) in ordered_provider_connections(
            store,
            &provider,
            requested,
            candidates,
            provider_order_cache,
            order_mode,
        )
        .await?
        {
            if let Some(desc) = registry::descriptor(&conn.provider) {
                out.push(AnnotatedRouteTarget {
                    target: RouteTarget {
                        conn,
                        desc,
                        upstream_model: requested.to_string(),
                        route_target_key: None,
                    },
                    reason: combine_order_reason(
                        cross_provider_ordered.then_some(RouteSelectionReason::Ordered),
                        account_reason,
                    ),
                });
            }
        }
    }
    Ok(filter_tool_compatible(
        normalize_single_reason(out),
        tool_requirements,
    ))
}

fn filter_tool_compatible(
    targets: Vec<AnnotatedRouteTarget>,
    requirements: capabilities::ToolTransportRequirements,
) -> Vec<AnnotatedRouteTarget> {
    targets
        .into_iter()
        .filter(|target| requirements.satisfied_by(target_tool_capabilities(&target.target)))
        .collect()
}

fn target_tool_capabilities(target: &RouteTarget) -> TransportToolCapabilities {
    target
        .desc
        .tool_transport
        .capabilities_for_endpoint(connections::endpoint_source(&target.conn))
}

fn combine_order_reason(
    outer: Option<RouteSelectionReason>,
    inner: RouteSelectionReason,
) -> RouteSelectionReason {
    if matches!(outer, Some(RouteSelectionReason::RoundRobin))
        || matches!(inner, RouteSelectionReason::RoundRobin)
    {
        RouteSelectionReason::RoundRobin
    } else if matches!(outer, Some(RouteSelectionReason::Ordered))
        || matches!(inner, RouteSelectionReason::Ordered)
    {
        RouteSelectionReason::Ordered
    } else {
        RouteSelectionReason::Initial
    }
}

fn normalize_single_reason(mut targets: Vec<AnnotatedRouteTarget>) -> Vec<AnnotatedRouteTarget> {
    if targets.len() == 1 {
        targets[0].reason = RouteSelectionReason::Initial;
    }
    targets
}

/// All providers are now drivable on the Anthropic-Messages / native path
/// (including `openai-oauth`/Codex, via [`codex_stream`]), so routing no
/// longer needs to exclude anything here. Kept as a named predicate — rather
/// than inlined `true` at each call site — so a future not-yet-drivable
/// provider has a single place to gate.
fn anthropic_messages_target_allowed(
    _conn: &connections::ConnectionRow,
    _desc: &ProviderDescriptor,
) -> bool {
    true
}

async fn expanded_route_targets(
    store: &Store,
    enabled: &[connections::ConnectionRow],
    target: &routes::ModelRouteTarget,
    target_allowed: fn(&connections::ConnectionRow, &ProviderDescriptor) -> bool,
    route_reason: RouteSelectionReason,
    provider_order_cache: &mut ProviderOrderCache,
    order_mode: RouteOrderMode,
) -> anyhow::Result<Vec<AnnotatedRouteTarget>> {
    let mut candidates = Vec::new();
    for conn in enabled {
        let Some(desc) = registry::descriptor(&conn.provider) else {
            continue;
        };
        if desc.family == target.provider
            && target_allowed(conn, desc)
            && connection_has_required_credentials(desc, conn)
            && connection_serves_model(desc, conn, &target.model, false)
        {
            candidates.push(conn.clone());
        }
    }
    let ordered = ordered_provider_connections(
        store,
        &target.provider,
        &target.model,
        candidates,
        provider_order_cache,
        order_mode,
    )
    .await?;
    Ok(ordered
        .into_iter()
        .filter_map(|(conn, account_reason)| {
            registry::descriptor(&conn.provider).map(|desc| AnnotatedRouteTarget {
                target: RouteTarget {
                    conn,
                    desc,
                    upstream_model: target.model.clone(),
                    route_target_key: None,
                },
                reason: combine_order_reason(Some(route_reason.clone()), account_reason),
            })
        })
        .collect())
}

/// Continuation targets once a `family/model`-pinned request has exhausted
/// every same-family account with retryable failures: the targets of the
/// first enabled Model Route (in `list_model_routes` order — the Models →
/// Route tab ordering) whose target list contains that exact (family, model)
/// pair, expanded to concrete connections in the route's CONFIGURED target
/// order, minus (connection id, model) pairs already attempted.
///
/// `route.targets` is used directly rather than `routes::ordered_targets`,
/// which would advance a RoundRobin route's cursor as a side effect of a
/// mere continuation lookup — the locked product decision is "continue in
/// the target order configured in the Route tab".
///
/// Empty when the request isn't `family/model`-pinned (named routes and bare
/// models don't continue) or no enabled route lists the pinned model.
async fn route_continuation_targets(
    store: &Store,
    requested: &str,
    attempted: &std::collections::HashSet<(String, String)>,
    provider_order_cache: &mut ProviderOrderCache,
    tool_requirements: capabilities::ToolTransportRequirements,
    order_mode: RouteOrderMode,
) -> anyhow::Result<Vec<AnnotatedRouteTarget>> {
    let Some((family, model)) = requested.split_once('/') else {
        return Ok(Vec::new());
    };
    let route_list = routes::list_model_routes(store).await?;
    let Some(route) = route_list.iter().find(|route| {
        route.enabled
            && route
                .targets
                .iter()
                .any(|t| t.provider == family && t.model == model)
    }) else {
        return Ok(Vec::new());
    };
    let conns = connections::list_connections(store).await?;
    let enabled: Vec<_> = conns.into_iter().filter(|c| c.enabled).collect();
    let mut seen = attempted.clone();
    let mut out = Vec::new();
    for target in &route.targets {
        for annotated in expanded_route_targets(
            store,
            &enabled,
            target,
            anthropic_messages_target_allowed,
            RouteSelectionReason::Ordered,
            provider_order_cache,
            order_mode,
        )
        .await?
        {
            let key = (
                annotated.target.conn.id.clone(),
                annotated.target.upstream_model.clone(),
            );
            if !seen.insert(key) {
                continue;
            }
            out.push(annotated);
        }
    }
    Ok(filter_tool_compatible(out, tool_requirements))
}

fn route_target_has_candidate(
    enabled: &[connections::ConnectionRow],
    target: &routes::ModelRouteTarget,
    target_allowed: fn(&connections::ConnectionRow, &ProviderDescriptor) -> bool,
) -> bool {
    enabled.iter().any(|conn| {
        registry::descriptor(&conn.provider)
            .map(|desc| {
                desc.family == target.provider
                    && target_allowed(conn, desc)
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
    (allow_unlisted && models.is_empty()) || models.iter().any(|served| served == model)
}

fn resolved_requested_model(
    conn: &connections::ConnectionRow,
    desc: &ProviderDescriptor,
    requested: &str,
    model: &str,
    allow_unlisted: bool,
) -> Option<String> {
    if connection_serves_model(desc, conn, model, allow_unlisted) {
        return Some(model.to_string());
    }
    if conn.provider != "openai-oauth" {
        return None;
    }
    let (canonical, _) = model_effort::parse_legacy_codex_selection(requested)?;
    let (family, canonical_model) = canonical
        .split_once('/')
        .map_or(("openai", canonical.as_str()), |(family, model)| {
            (family, model)
        });
    if family != "openai" || desc.family != family {
        return None;
    }
    let models = connections::effective_models(desc, conn);
    let known = models.iter().any(|served| served == canonical_model)
        || canonical_model
            .strip_suffix("-review")
            .is_some_and(|base| models.iter().any(|served| served == base));
    known.then(|| canonical_model.to_string())
}

fn resolve_target_effort(
    policy: &model_effort::TurnEffortPolicy,
    route_target_key: Option<&model_effort::RouteTargetEffortKey>,
    preference_key: &model_effort::ModelPreferenceKey,
    surface: &model_effort::ExecutionSurfaceKey,
) -> model_effort::EffectiveEffort {
    model_effort::resolve_for_target(policy, route_target_key, preference_key, surface)
}

async fn ordered_provider_connections(
    store: &Store,
    provider: &str,
    scope: &str,
    candidates: Vec<connections::ConnectionRow>,
    provider_order_cache: &mut ProviderOrderCache,
    order_mode: RouteOrderMode,
) -> anyhow::Result<Vec<(connections::ConnectionRow, RouteSelectionReason)>> {
    if candidates.len() <= 1 {
        return Ok(candidates
            .into_iter()
            .map(|conn| (conn, RouteSelectionReason::Initial))
            .collect());
    }
    let ids = candidates
        .iter()
        .map(|conn| conn.id.clone())
        .collect::<Vec<_>>();
    let cache_key = (provider.to_string(), scope.to_string(), ids.clone());
    let (ordered_ids, reason) = if let Some(cached) = provider_order_cache.get(&cache_key) {
        cached.clone()
    } else {
        let (ordered_ids, strategy) = match order_mode {
            RouteOrderMode::Advance => {
                routes::ordered_provider_connection_ids_with_strategy(store, provider, scope, &ids)
                    .await?
            }
            RouteOrderMode::Peek => (
                routes::peek_provider_connection_ids(store, provider, scope, &ids).await?,
                routes::provider_account_route(store, provider)
                    .await?
                    .strategy,
            ),
        };
        let reason = if strategy == routes::ModelRouteStrategy::RoundRobin {
            RouteSelectionReason::RoundRobin
        } else {
            RouteSelectionReason::Ordered
        };
        provider_order_cache.insert(cache_key, (ordered_ids.clone(), reason.clone()));
        (ordered_ids, reason)
    };
    let mut by_id = candidates
        .into_iter()
        .map(|conn| (conn.id.clone(), conn))
        .collect::<std::collections::HashMap<_, _>>();
    Ok(ordered_ids
        .into_iter()
        .filter_map(|id| by_id.remove(&id).map(|conn| (conn, reason.clone())))
        .collect())
}

fn prefer_capable_indexed_targets(
    targets: Vec<routes::IndexedModelRouteTarget>,
    required: capabilities::RequiredCapabilities,
) -> Vec<routes::IndexedModelRouteTarget> {
    if !required.any() || targets.len() <= 1 {
        return targets;
    }
    let mut capable = Vec::new();
    let mut rest = Vec::new();
    for target in targets {
        if required.satisfied_by(capabilities::model_capabilities(&target.target.model)) {
            capable.push(target);
        } else {
            rest.push(target);
        }
    }
    capable.extend(rest);
    capable
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
            return Some(format!("{}/{}", desc.family, model));
        }
    }
    None
}

/// Default model for the native runtime / Anthropic Messages client path.
/// Prefer named routes so user-created combo aliases become the natural
/// native default.
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
            return Some(format!("{}/{}", desc.family, model));
        }
    }
    None
}

/// Whether the in-process native client can actually drive `conn` on the
/// Anthropic-Messages path. Reachable in-process:
///   * any api-key / no-auth connection (generic `/messages` or `/chat/
///     completions` wiring in [`upstream_request`], with OpenAI↔Anthropic
///     translation),
///   * the `anthropic-oauth` Claude subscription,
///   * `kiro` (AWS CodeWhisperer) via [`kiro_stream`] — the same
///     EventStream→OpenAI→Anthropic translation the endpoint server uses, and
///   * `openai-oauth` (Codex) via [`codex_stream`] — an Anthropic→OpenAI-chat→
///     Responses request translation, with the Responses SSE decoded back
///     into Anthropic events.
fn native_client_can_drive(conn: &connections::ConnectionRow) -> bool {
    if !connections::is_oauth(conn) {
        return true;
    }
    matches!(
        conn.provider.as_str(),
        "anthropic-oauth" | "kiro" | "openai-oauth"
    )
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
/// the picker never offers an entry that would error on send. Codex
/// (`openai-oauth`) effort capabilities are returned as structured metadata
/// on the canonical model entry rather than as synthetic model IDs.
pub async fn selectable_native_models(
    store: &Store,
) -> anyhow::Result<Vec<model_effort::SelectableModelInfo>> {
    let enabled = connections::list_connections(store)
        .await?
        .into_iter()
        .filter(|connection| connection.enabled)
        .collect::<Vec<_>>();
    let configured = store
        .list_model_effort_preferences()
        .await?
        .into_iter()
        .collect::<std::collections::HashMap<_, _>>();

    let capability = |connection: &connections::ConnectionRow, family: String, model: String| {
        let surface = model_effort::ExecutionSurfaceKey {
            provider_id: connection.provider.clone(),
            connection_id: Some(connection.id.clone()),
            model: model.clone(),
        };
        async move {
            let capabilities =
                model_capabilities::resolve_for_surface(store, &family, &surface).await;
            (
                model_effort::ModelPreferenceKey {
                    family,
                    model: model.clone(),
                },
                capabilities,
            )
        }
    };

    let mut concrete_order = Vec::<model_effort::ModelPreferenceKey>::new();
    let mut concrete = std::collections::HashMap::<
        model_effort::ModelPreferenceKey,
        Vec<model_effort::ExecutionModelEffortCapabilities>,
    >::new();
    for connection in &enabled {
        let Some(descriptor) = registry::descriptor(&connection.provider) else {
            continue;
        };
        if !native_client_can_drive(connection)
            || !connection_has_required_credentials(descriptor, connection)
        {
            continue;
        }
        for model in connections::effective_models(descriptor, connection) {
            let (key, capabilities) =
                capability(connection, descriptor.family.to_string(), model).await;
            if !concrete.contains_key(&key) {
                concrete_order.push(key.clone());
            }
            concrete.entry(key).or_default().push(capabilities);
        }
    }

    let make_info =
        |kind,
         request_value: String,
         display_name: String,
         preference_key: Option<model_effort::ModelPreferenceKey>,
         configured_override: Option<String>,
         capabilities: Vec<model_effort::ExecutionModelEffortCapabilities>| {
            let intersection = model_effort::intersect_capabilities(&capabilities);
            let configured_default = configured_override
                .or_else(|| {
                    preference_key
                        .as_ref()
                        .and_then(|key| configured.get(key))
                        .cloned()
                })
                .filter(|value| {
                    intersection
                        .supported
                        .iter()
                        .any(|option| option.value == value.as_str())
                });
            let (resolved_default, default_source) = configured_default.clone().map_or(
                (intersection.resolved_default, intersection.default_source),
                |value| (Some(value), model_effort::ModelDefaultSource::Configured),
            );
            model_effort::SelectableModelInfo {
                kind,
                request_value,
                display_name,
                preference_key,
                supported: intersection.supported,
                configured_default,
                resolved_default,
                default_source,
            }
        };

    let mut out = Vec::new();
    for route in routes::list_model_routes(store)
        .await?
        .into_iter()
        .filter(|route| route.enabled && !route.targets.is_empty())
    {
        let mut route_capabilities = Vec::new();
        let mut route_effective = Vec::new();
        for target in routes::peek_ordered_targets(store, &route).await? {
            for connection in &enabled {
                let Some(descriptor) = registry::descriptor(&connection.provider) else {
                    continue;
                };
                if descriptor.family == target.provider
                    && native_client_can_drive(connection)
                    && connection_has_required_credentials(descriptor, connection)
                    && connection_serves_model(descriptor, connection, &target.model, false)
                {
                    let (key, capability) = capability(
                        connection,
                        descriptor.family.to_string(),
                        target.model.clone(),
                    )
                    .await;
                    let supports = |value: &str| {
                        capability
                            .supported
                            .iter()
                            .any(|option| option.value == value)
                    };
                    let selected = target
                        .effort
                        .as_deref()
                        .filter(|value| supports(value))
                        .map(|value| {
                            (
                                Some(value.to_string()),
                                model_effort::EffectiveEffortSource::RouteTarget,
                            )
                        })
                        .or_else(|| {
                            configured
                                .get(&key)
                                .filter(|value| supports(value))
                                .map(|value| {
                                    (
                                        Some(value.clone()),
                                        model_effort::EffectiveEffortSource::Configured,
                                    )
                                })
                        })
                        .or_else(|| {
                            capability
                                .provider_default
                                .as_ref()
                                .filter(|value| supports(value))
                                .cloned()
                                .map(|value| {
                                    (Some(value), model_effort::EffectiveEffortSource::Provider)
                                })
                        })
                        .unwrap_or((None, model_effort::EffectiveEffortSource::None));
                    route_effective.push(selected);
                    route_capabilities.push(capability);
                }
            }
        }
        if !route_capabilities.is_empty() {
            let configured_values = route_capabilities
                .iter()
                .map(|capability| {
                    let family = registry::descriptor(&capability.surface.provider_id)
                        .map(|descriptor| descriptor.family)?;
                    configured
                        .get(&model_effort::ModelPreferenceKey {
                            family: family.to_string(),
                            model: capability.surface.model.clone(),
                        })
                        .cloned()
                })
                .collect::<Vec<_>>();
            let route_configured = configured_values
                .first()
                .cloned()
                .flatten()
                .filter(|first| {
                    configured_values
                        .iter()
                        .all(|value| value.as_ref() == Some(first))
                });
            let mut info = make_info(
                model_effort::SelectableModelKind::NamedRoute,
                route.name.clone(),
                route.name,
                None,
                route_configured,
                route_capabilities,
            );
            let first_value = route_effective.first().map(|(value, _)| value.clone());
            let uniform_value = first_value.is_some()
                && route_effective
                    .iter()
                    .all(|(value, _)| Some(value.clone()) == first_value);
            if uniform_value {
                info.resolved_default = first_value.flatten();
                let first_source = route_effective.first().map(|(_, source)| source);
                let uniform_source = first_source.is_some()
                    && route_effective
                        .iter()
                        .all(|(_, source)| Some(source) == first_source);
                info.default_source = if uniform_source {
                    match first_source {
                        Some(model_effort::EffectiveEffortSource::Configured) => {
                            model_effort::ModelDefaultSource::Configured
                        }
                        Some(model_effort::EffectiveEffortSource::Provider) => {
                            model_effort::ModelDefaultSource::Provider
                        }
                        Some(model_effort::EffectiveEffortSource::None) => {
                            model_effort::ModelDefaultSource::None
                        }
                        _ => model_effort::ModelDefaultSource::VariesByTarget,
                    }
                } else {
                    model_effort::ModelDefaultSource::VariesByTarget
                };
            } else {
                info.resolved_default = None;
                info.default_source = model_effort::ModelDefaultSource::VariesByTarget;
            }
            out.push(info);
        }
    }
    for key in concrete_order {
        let mut capabilities = concrete.remove(&key).unwrap_or_default();
        let ids = capabilities
            .iter()
            .filter_map(|capability| capability.surface.connection_id.clone())
            .collect::<Vec<_>>();
        let ordered_ids =
            routes::peek_provider_connection_ids(store, &key.family, &key.model, &ids).await?;
        let order = ordered_ids
            .into_iter()
            .enumerate()
            .map(|(index, id)| (id, index))
            .collect::<std::collections::HashMap<_, _>>();
        capabilities.sort_by_key(|capability| {
            capability
                .surface
                .connection_id
                .as_ref()
                .and_then(|id| order.get(id))
                .copied()
                .unwrap_or(usize::MAX)
        });
        let display_name = if key.model.ends_with("-review") {
            key.model.clone()
        } else {
            capabilities
                .first()
                .map(|capability| capability.model_display_name.clone())
                .unwrap_or_else(|| key.model.clone())
        };
        out.push(make_info(
            model_effort::SelectableModelKind::Concrete,
            format!("{}/{}", key.family, key.model),
            display_name,
            Some(key),
            None,
            capabilities,
        ));
    }
    Ok(out)
}

pub async fn named_route_target_default(
    store: &Store,
    requested: &str,
    supported: &[model_effort::ReasoningEffortOption],
) -> anyhow::Result<Option<String>> {
    let route_list = routes::list_model_routes(store).await?;
    let Some(route) = routes::route_by_name(&route_list, requested) else {
        return Ok(None);
    };
    let enabled = connections::list_connections(store)
        .await?
        .into_iter()
        .filter(|connection| connection.enabled)
        .collect::<Vec<_>>();
    let efforts = route
        .targets
        .iter()
        .filter(|target| {
            route_target_has_candidate(&enabled, target, anthropic_messages_target_allowed)
        })
        .map(|target| target.effort.clone())
        .collect::<Vec<_>>();
    let Some(first) = efforts.first().cloned().flatten() else {
        return Ok(None);
    };
    if !efforts
        .iter()
        .all(|effort| effort.as_deref() == Some(first.as_str()))
        || !supported.iter().any(|option| option.value == first)
    {
        return Ok(None);
    }
    Ok(Some(first))
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
    claude_cloak::tool_name_map_for(&target.conn.provider, body)
}

/// Remove the `thinking` key before a request reaches an Anthropic-native
/// upstream (the `/messages` passthrough and kiro's translated-but-still-
/// Claude-shaped payload). Extended thinking is not yet supported on these
/// upstreams: the native runner does not replay signed thinking blocks in
/// tool-use continuations, and the newest Anthropic models reject
/// `budget_tokens` outright. The OpenAI-translation path
/// (`translate::anthropic_to_openai_request`, used directly by the OpenAI
/// format and by Codex's Responses-API bridge) maps this key to
/// `reasoning_effort` instead and is unaffected — remove this gate once
/// thinking-block replay lands in the runner.
fn strip_thinking(body: &mut Value) {
    if let Some(obj) = body.as_object_mut() {
        obj.remove("thinking");
    }
}

pub(crate) fn strip_kiro_effort(body: &mut Value) {
    if let Some(output) = body.get_mut("output_config").and_then(Value::as_object_mut) {
        output.remove("effort");
    }
    if let Some(reasoning) = body.get_mut("reasoning").and_then(Value::as_object_mut) {
        reasoning.remove("effort");
    }
    if let Some(object) = body.as_object_mut() {
        object.remove("reasoning_effort");
    }
}

pub(crate) fn target_effort(
    target: &RouteTarget,
    policy: &model_effort::TurnEffortPolicy,
) -> model_effort::EffectiveEffort {
    let preference_key = model_effort::ModelPreferenceKey {
        family: target.desc.family.to_string(),
        model: target.upstream_model.clone(),
    };
    let surface = model_effort::ExecutionSurfaceKey {
        provider_id: target.conn.provider.clone(),
        connection_id: Some(target.conn.id.clone()),
        model: target.upstream_model.clone(),
    };
    resolve_target_effort(
        policy,
        target.route_target_key.as_ref(),
        &preference_key,
        &surface,
    )
}

fn selection_for_accepted_target(
    target: &RouteTarget,
    requested_model: &str,
    policy: &model_effort::TurnEffortPolicy,
    reason: RouteSelectionReason,
) -> RouteSelection {
    let preference_key = model_effort::ModelPreferenceKey {
        family: target.desc.family.to_string(),
        model: target.upstream_model.clone(),
    };
    let surface = model_effort::ExecutionSurfaceKey {
        provider_id: target.conn.provider.clone(),
        connection_id: Some(target.conn.id.clone()),
        model: target.upstream_model.clone(),
    };
    let effective = model_effort::resolve_for_target(
        policy,
        target.route_target_key.as_ref(),
        &preference_key,
        &surface,
    );
    let model_display_name = policy
        .surfaces
        .get(&surface)
        .map(|capability| capability.model_display_name.clone())
        .unwrap_or_else(|| target.upstream_model.clone());
    RouteSelection {
        requested_model: requested_model.to_string(),
        resolved_provider_id: target.conn.provider.clone(),
        resolved_family: target.desc.family.to_string(),
        resolved_model: target.upstream_model.clone(),
        resolved_model_display_name: model_display_name,
        effective_effort: effective.value,
        effective_effort_label: effective.label,
        connection_id: target.conn.id.clone(),
        connection_label: target.conn.label.clone(),
        reason,
    }
}

fn accepted_reason(
    origin: RouteSelectionReason,
    failures: &[UpstreamAttemptFailure],
) -> RouteSelectionReason {
    failures
        .last()
        .map(|failure| RouteSelectionReason::Failover(failure.category))
        .unwrap_or(origin)
}

pub(crate) fn apply_anthropic_effort(
    body: &mut Value,
    target: &RouteTarget,
    policy: &model_effort::TurnEffortPolicy,
) {
    if let Some(effort) = target_effort(target, policy).value {
        body["output_config"]["effort"] = json!(effort);
    } else if let Some(output) = body.get_mut("output_config").and_then(Value::as_object_mut) {
        output.remove("effort");
    }
}

pub(crate) fn apply_openai_effort(
    body: &mut Value,
    target: &RouteTarget,
    policy: &model_effort::TurnEffortPolicy,
    caller_supplied_thinking: bool,
) {
    if let Some(effort) = target_effort(target, policy).value {
        body["reasoning_effort"] = json!(effort);
    } else if target.route_target_key.is_some() || !caller_supplied_thinking {
        if let Some(object) = body.as_object_mut() {
            object.remove("reasoning_effort");
        }
    }
}

// ---------------------------------------------------------------------------
// Upstream request construction (moved from server.rs — behavior unchanged,
// `&AppState` retargeted to `&UpstreamCtx`)
// ---------------------------------------------------------------------------

/// Chat-generation path appended to the effective base URL. Most providers
/// follow their wire format's standard path; `chat_path` overrides it for
/// endpoints with nonstandard shapes.
fn upstream_chat_path(desc: &ProviderDescriptor) -> &'static str {
    if let Some(path) = desc.chat_path {
        return path;
    }
    match desc.format {
        ApiFormat::OpenAi => "/chat/completions",
        ApiFormat::Anthropic => "/messages",
    }
}

/// OpenAI's current generation (gpt-5.x / o-series) rejects `max_tokens` with
/// HTTP 400 and requires `max_completion_tokens`. Applied post-translation at
/// call sites that know the descriptor, so `translate` stays
/// provider-agnostic. A no-op for every other provider (mimo, qwen, copilot,
/// custom-openai, … all still speak `max_tokens`).
pub(crate) fn apply_max_completion_tokens(desc: &ProviderDescriptor, body: &mut Value) {
    if !desc.uses_max_completion_tokens {
        return;
    }
    if let Some(obj) = body.as_object_mut() {
        if let Some(v) = obj.remove("max_tokens") {
            obj.insert("max_completion_tokens".to_string(), v);
        }
    }
}

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
    let path = upstream_chat_path(target.desc);
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

/// Extra per-provider request headers keyed by provider id. Copilot's chat
/// endpoint requires a VS Code / Copilot-Chat fingerprint; opencode-free wants
/// its placeholder bearer + client id. Applied on the OAuth (Copilot) and
/// free (opencode) paths.
fn apply_provider_request_headers(
    req: reqwest::RequestBuilder,
    provider: &str,
) -> reqwest::RequestBuilder {
    match provider {
        "opencode-free" => req
            .header("authorization", "Bearer public")
            .header("x-opencode-client", "desktop"),
        "github-copilot" => req
            .header("copilot-integration-id", "vscode-chat")
            .header("editor-version", "vscode/1.110.0")
            .header("editor-plugin-version", "copilot-chat/0.38.0")
            .header("user-agent", "GitHubCopilotChat/0.38.0")
            .header("openai-intent", "conversation-panel")
            .header("x-github-api-version", "2025-04-01")
            .header("x-vscode-user-agent-library-version", "electron-fetch")
            .header("x-initiator", "user")
            .header("x-request-id", uuid::Uuid::new_v4().to_string()),
        _ => req,
    }
}

/// Qwen chat base: tokens are bound to the shard `resource_url` returned at
/// grant time; using portal.qwen.ai for a token issued on another shard 401s.
/// Falls back to the descriptor base when no `resource_url` is present.
fn qwen_base_url(target: &RouteTarget) -> anyhow::Result<String> {
    if let Some(host) = target
        .conn
        .data
        .provider_specific
        .as_ref()
        .and_then(|v| v.get("resource_url"))
        .and_then(|v| v.as_str())
        .map(|s| {
            s.trim_start_matches("https://")
                .trim_start_matches("http://")
                .trim_end_matches('/')
        })
        .filter(|s| !s.is_empty())
    {
        return Ok(format!("https://{host}/v1"));
    }
    connections::effective_base_url(target.desc, &target.conn)
        .ok_or_else(|| anyhow::anyhow!("connection {} has no base URL", target.conn.id))
}

/// GitHub Copilot's `/chat/completions` only accepts `text` and `image_url`
/// content parts; any other part type (`tool_use`/`tool_result`/`thinking`/…)
/// 400s. Serialize those to a `text` part so tool-using harness sessions work.
/// String content and top-level `tool_calls` are left untouched.
fn sanitize_copilot_body(body: &mut Value) {
    let Some(messages) = body.get_mut("messages").and_then(|m| m.as_array_mut()) else {
        return;
    };
    for msg in messages.iter_mut() {
        let Some(parts) = msg.get_mut("content").and_then(|c| c.as_array_mut()) else {
            continue; // string content or no content — leave as-is.
        };
        for part in parts.iter_mut() {
            let kind = part.get("type").and_then(|t| t.as_str()).unwrap_or("");
            if kind == "text" || kind == "image_url" {
                continue;
            }
            let serialized = serde_json::to_string(part).unwrap_or_default();
            *part = serde_json::json!({ "type": "text", "text": serialized });
        }
    }
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
            let cloaked = claude_cloak::required_for_provider(&target.conn.provider);
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
            #[cfg(test)]
            let codex_base = target
                .conn
                .data
                .base_url_override
                .as_deref()
                .unwrap_or(CODEX_BASE);
            #[cfg(not(test))]
            let codex_base = CODEX_BASE;
            let mut req = ctx
                .http
                .post(format!("{codex_base}/responses"))
                .json(body)
                .header("authorization", format!("Bearer {access_token}"))
                .header("originator", "codex_cli_rs")
                // The Codex CLI identifies itself with these on every request
                // (9router `providers/registry/codex.js`); the Responses wire
                // always streams, so Accept is text/event-stream.
                .header("user-agent", "codex_cli_rs/0.136.0")
                .header("accept", "text/event-stream")
                .header("session_id", uuid::Uuid::new_v4().to_string());
            if let Some(account_id) = crate::llm_router::models::chatgpt_account_id(&target.conn) {
                req = req.header("chatgpt-account-id", account_id);
            }
            Ok(req)
        }
        "qwen" => {
            let base = qwen_base_url(target)?;
            let req = ctx
                .http
                .post(format!("{base}/chat/completions"))
                .json(body)
                .header("authorization", format!("Bearer {access_token}"));
            Ok(req)
        }
        "github-copilot" => {
            let base = connections::effective_base_url(target.desc, &target.conn)
                .ok_or_else(|| anyhow::anyhow!("connection {} has no base URL", target.conn.id))?;
            let mut copilot_body = body.clone();
            sanitize_copilot_body(&mut copilot_body);
            let req = ctx
                .http
                .post(format!("{base}/chat/completions"))
                .json(&copilot_body)
                .header("authorization", format!("Bearer {access_token}"));
            Ok(apply_provider_request_headers(req, &target.conn.provider))
        }
        other => Err(anyhow::anyhow!(
            "no OAuth upstream wiring for provider `{other}`"
        )),
    }
}

/// Free-tier passthrough: no real credential. opencode-free additionally
/// wants its wire's placeholder bearer + client-id header; other no_auth
/// providers (mimo-free) get a bare JSON POST.
fn free_upstream_request(
    ctx: &UpstreamCtx,
    target: &RouteTarget,
    body: &Value,
) -> anyhow::Result<reqwest::RequestBuilder> {
    let base = connections::effective_base_url(target.desc, &target.conn)
        .ok_or_else(|| anyhow::anyhow!("connection {} has no base URL", target.conn.id))?;
    let path = upstream_chat_path(target.desc);
    // MiMo's free tier sits behind an anti-abuse gate: bootstrap-JWT bearer,
    // Chrome-like UA + fingerprint headers, and the MiMoCode marker system
    // message (see `mimo`). The bearer is attached from the process cache —
    // async callers mint it via `mimo::ensure_jwt` before sending, and a
    // missing token simply 403s into `send_upstream`'s re-bootstrap retry.
    if target.conn.provider == "mimo-free" {
        let mut gated = body.clone();
        mimo::inject_system_marker(&mut gated);
        // The MiMoCode CLI sends Accept matching the stream mode
        // (9router `executors/mimo-free.js` buildHeaders).
        let accept = if gated
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false)
        {
            "text/event-stream"
        } else {
            "application/json"
        };
        let mut req = ctx
            .http
            .post(format!("{base}{path}"))
            .json(&gated)
            .header("user-agent", mimo::CHROME_UA)
            .header("x-mimo-source", "mimocode-cli-free")
            .header("x-session-affinity", mimo::session_affinity())
            .header("accept", accept);
        if let Some(jwt) = mimo::cached_jwt() {
            req = req.header("authorization", format!("Bearer {jwt}"));
        }
        return Ok(req);
    }
    let req = ctx.http.post(format!("{base}{path}")).json(body);
    Ok(apply_provider_request_headers(req, &target.conn.provider))
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
    if target.conn.provider == "mimo-free" {
        // Best-effort: a bootstrap outage falls through to an
        // unauthenticated request whose 403 hits the retry below.
        let _ = mimo::ensure_jwt(&ctx.http, ctx.mimo_bootstrap_url_override.as_deref()).await;
    }
    let resp = upstream_request(ctx, target, body)?.send().await?;
    if matches!(resp.status().as_u16(), 401 | 403) && target.conn.provider == "mimo-free" {
        // The upstream rejected the cached bootstrap JWT — mint a fresh one
        // and retry the same request once, mirroring the OAuth path below.
        mimo::invalidate_jwt();
        if mimo::ensure_jwt(&ctx.http, ctx.mimo_bootstrap_url_override.as_deref())
            .await
            .is_ok()
        {
            return Ok(upstream_request(ctx, target, body)?.send().await?);
        }
        return Ok(resp);
    }
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
pub(crate) struct UpstreamAttemptFailure {
    pub(crate) provider: String,
    pub(crate) message: String,
    pub(crate) status: Option<u16>,
    pub(crate) category: RouteFailureCategory,
}

impl UpstreamAttemptFailure {
    /// Build a transport-class failure (no HTTP status, always retryable).
    pub(crate) fn transport(provider: impl Into<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            provider: provider.into(),
            category: classify_failure(None, true, &message),
            message,
            status: None,
        }
    }

    pub(crate) fn http(
        provider: impl Into<String>,
        status: u16,
        message: impl Into<String>,
    ) -> Self {
        let message = message.into();
        Self {
            provider: provider.into(),
            category: classify_failure(Some(status), false, &message),
            message,
            status: Some(status),
        }
    }

    fn upstream(provider: impl Into<String>, message: impl Into<String>) -> Self {
        let message = message.into();
        Self {
            provider: provider.into(),
            category: classify_failure(None, false, &message),
            message,
            status: None,
        }
    }

    fn display(&self) -> String {
        format!("[{}] {}", self.provider, self.message)
    }
}

pub(crate) fn should_try_next_target(failure: &UpstreamAttemptFailure) -> bool {
    if matches!(
        failure.category,
        RouteFailureCategory::Transport
            | RouteFailureCategory::Authentication
            | RouteFailureCategory::Quota
            | RouteFailureCategory::RateLimit
    ) {
        return true;
    }
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

pub(crate) fn fallback_error(
    requested: &str,
    failures: &[UpstreamAttemptFailure],
) -> anyhow::Error {
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

pub(crate) async fn ensure_fresh_for_attempt(
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
            category: RouteFailureCategory::Authentication,
        });
    }
    Ok(())
}

pub(crate) async fn upstream_status_failure(
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
    UpstreamAttemptFailure::http(provider, status, message)
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
        /// Authoritative request input tokens when the upstream reports them in
        /// the terminal delta (OpenAI-format translation, Task 1). `None` = not
        /// reported here (Anthropic upstreams carry input on message_start).
        input_tokens: Option<i64>,
        cache_read_tokens: Option<i64>,
        cache_creation_tokens: Option<i64>,
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
            "message_delta" => {
                let usage = data.get("usage");
                let opt = |key: &str| -> Option<i64> {
                    usage
                        .and_then(|u| u.get(key))
                        .and_then(|v| v.as_i64())
                        .filter(|v| *v > 0)
                };
                Some(MessageStreamEvent::MessageDelta {
                    stop_reason: data
                        .get("delta")
                        .and_then(|d| d.get("stop_reason"))
                        .and_then(|v| v.as_str())
                        .map(|s| s.to_string()),
                    output_tokens: opt("output_tokens").unwrap_or(0),
                    input_tokens: opt("input_tokens"),
                    cache_read_tokens: opt("cache_read_input_tokens"),
                    cache_creation_tokens: opt("cache_creation_input_tokens"),
                })
            }
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

/// Outcome of probing the head of an attempt's event stream before anything
/// has been handed to the consumer.
enum StreamProbe {
    /// Content (or a clean end) was observed — deliver `buffered` then `rest`.
    Deliver {
        buffered: Vec<anyhow::Result<AnthropicEvent>>,
        rest: mpsc::Receiver<anyhow::Result<AnthropicEvent>>,
    },
    /// The stream failed before producing any content — safe to fail over.
    Failover(UpstreamAttemptFailure),
}

/// Watch the first events of a freshly-started attempt stream. Nothing has
/// been forwarded to the consumer yet, so an error arriving before the first
/// content event (`content_block_start`/`content_block_delta`) can still be
/// converted into an attempt failure and the next target tried. From the
/// first content event on, the stream belongs to the consumer — later errors
/// flow through verbatim (failing over then would duplicate output).
async fn probe_stream_head(
    provider: &str,
    mut rx: mpsc::Receiver<anyhow::Result<AnthropicEvent>>,
) -> StreamProbe {
    let mut buffered: Vec<anyhow::Result<AnthropicEvent>> = Vec::new();
    loop {
        let Some(item) = rx.recv().await else {
            // Ended without content and without an error event — a valid
            // (empty) completion; deliver whatever arrived.
            return StreamProbe::Deliver { buffered, rest: rx };
        };
        let failure = match &item {
            Err(e) => Some(UpstreamAttemptFailure::transport(provider, e.to_string())),
            Ok((name, data)) if name == "error" => Some(UpstreamAttemptFailure::upstream(
                provider,
                data.get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                    .unwrap_or("upstream error"),
            )),
            Ok(_) => None,
        };
        if let Some(failure) = failure {
            return StreamProbe::Failover(failure);
        }
        let is_content = matches!(
            &item,
            Ok((name, _)) if name == "content_block_start" || name == "content_block_delta"
        );
        buffered.push(item);
        if is_content {
            return StreamProbe::Deliver { buffered, rest: rx };
        }
    }
}

/// Re-join a probed stream: emit the buffered head events, then forward the
/// rest of the upstream receiver.
fn deliver_probed(
    buffered: Vec<anyhow::Result<AnthropicEvent>>,
    mut rest: mpsc::Receiver<anyhow::Result<AnthropicEvent>>,
) -> mpsc::Receiver<anyhow::Result<AnthropicEvent>> {
    if buffered.is_empty() {
        return rest;
    }
    let (tx, out) = mpsc::channel(64);
    tokio::spawn(async move {
        for item in buffered {
            if tx.send(item).await.is_err() {
                return;
            }
        }
        while let Some(item) = rest.recv().await {
            if tx.send(item).await.is_err() {
                return;
            }
        }
    });
    out
}

/// Route + send an Anthropic-Messages-format request exactly like
/// `/v1/messages` (stream forced on) and yield Anthropic SSE events. The
/// returned channel closes when the upstream stream ends. Both Anthropic-
/// format upstreams (events forwarded directly) and OpenAI-format upstreams
/// (request translated, chunks re-encoded via
/// [`translate::OpenAiToAnthropicStream`]) produce the same event shape.
///
/// Failover: errors surfaced BEFORE streaming (routing miss, dead OAuth
/// token, transport send failure, non-2xx status) rotate to the next target,
/// and each attempt's stream head is probed ([`probe_stream_head`]) so an
/// error arriving before the first content event rotates too. Once content
/// has been delivered, a later error is delivered in-stream — failing over
/// then would duplicate output.
///
/// Route continuation: for family/model-pinned requests (`"provider/model"`),
/// once every same-family target from the initial routing pass has failed
/// retryably, routing continues once down the first matching Model Route's
/// targets, tried in their configured order while skipping any (connection,
/// model) pair already attempted.
pub async fn anthropic_messages_stream(
    ctx: &UpstreamCtx,
    body: Value,
    effort_policy: &model_effort::TurnEffortPolicy,
) -> anyhow::Result<RoutedStream> {
    let requested = body["model"].as_str().unwrap_or("").to_string();
    let tool_requirements = capabilities::tool_transport_requirements_from_body(&body);
    let mut provider_order_cache = ProviderOrderCache::new();
    let targets = filter_tool_compatible(
        route_models_for_body_matching_with_cache(
            &ctx.store,
            &requested,
            None,
            anthropic_messages_target_allowed,
            &mut provider_order_cache,
            RouteOrderMode::Advance,
        )
        .await?,
        tool_requirements,
    );
    if targets.is_empty() {
        anyhow::bail!("no enabled connection serves model '{requested}'");
    }
    let mut failures = Vec::new();
    let mut attempted = std::collections::HashSet::<(String, String)>::new();
    let mut queue = std::collections::VecDeque::from(targets);
    let mut continued = false;
    loop {
        let Some(annotated) = queue.pop_front() else {
            // Initial targets exhausted with only retryable failures
            // (non-retryable ones returned early above): continue down the
            // first Model Route containing the pinned (family, model) — once.
            if continued {
                break;
            }
            continued = true;
            queue.extend(
                route_continuation_targets(
                    &ctx.store,
                    &requested,
                    &attempted,
                    &mut provider_order_cache,
                    tool_requirements,
                    RouteOrderMode::Advance,
                )
                .await?,
            );
            if queue.is_empty() {
                break;
            }
            continue;
        };
        let mut target = annotated.target;
        let origin = annotated.reason;
        attempted.insert((target.conn.id.clone(), target.upstream_model.clone()));
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
            strip_thinking(&mut attempt_body);
            strip_kiro_effort(&mut attempt_body);
            match kiro_stream(ctx, &mut target, &attempt_body, started).await {
                Ok(rx) => match probe_stream_head(&target.conn.provider, rx).await {
                    StreamProbe::Deliver { buffered, rest } => {
                        let selection = selection_for_accepted_target(
                            &target,
                            &requested,
                            effort_policy,
                            accepted_reason(origin, &failures),
                        );
                        return Ok(RoutedStream {
                            selection,
                            events: deliver_probed(buffered, rest),
                        });
                    }
                    StreamProbe::Failover(failure) => {
                        failures.push(failure);
                        continue;
                    }
                },
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

        // Codex speaks the Responses API, not `/messages` or `/chat/
        // completions`, so — like Kiro — it's handled before the format
        // match via its own translation + upstream pipeline.
        if target.conn.provider == "openai-oauth" {
            match codex_stream(ctx, &mut target, &attempt_body, effort_policy, started).await {
                Ok(rx) => match probe_stream_head(&target.conn.provider, rx).await {
                    StreamProbe::Deliver { buffered, rest } => {
                        let selection = selection_for_accepted_target(
                            &target,
                            &requested,
                            effort_policy,
                            accepted_reason(origin, &failures),
                        );
                        return Ok(RoutedStream {
                            selection,
                            events: deliver_probed(buffered, rest),
                        });
                    }
                    StreamProbe::Failover(failure) => {
                        failures.push(failure);
                        continue;
                    }
                },
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

        // A connection backed by an installed WASM provider bundle diverts to
        // the in-process component, BEFORE the generic HTTP `match
        // target.desc.format` — the same choke point kiro/openai-oauth use. The
        // predicate is DATA-driven (a lookup by provider id in the registered
        // WASM providers), so no plugin id is hardcoded and the divert stays
        // generic. A trapping/looping `complete` is caught by the component's
        // fuel/epoch budget and surfaces as a route-scoped failure, never a
        // daemon crash.
        if let Some(transport) = crate::plugins::wasm_provider::wasm_provider(&target.conn.provider)
        {
            match wasm_provider_stream(ctx, &mut target, &attempt_body, transport, started).await {
                Ok(rx) => match probe_stream_head(&target.conn.provider, rx).await {
                    StreamProbe::Deliver { buffered, rest } => {
                        let selection = selection_for_accepted_target(
                            &target,
                            &requested,
                            effort_policy,
                            accepted_reason(origin, &failures),
                        );
                        return Ok(RoutedStream {
                            selection,
                            events: deliver_probed(buffered, rest),
                        });
                    }
                    StreamProbe::Failover(failure) => {
                        failures.push(failure);
                        continue;
                    }
                },
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
                strip_thinking(&mut attempt_body);
                apply_anthropic_effort(&mut attempt_body, &target, effort_policy);
                let tool_map = claude_cloak_map_for(&target, &attempt_body);
                let resp = match send_upstream(ctx, &mut target, &attempt_body).await {
                    Ok(resp) => resp,
                    Err(e) => {
                        // Transport-level send failure (DNS/TLS/connect/reset):
                        // record it and rotate instead of failing the call.
                        failures.push(UpstreamAttemptFailure::transport(
                            provider,
                            format!("upstream send failed: {e}"),
                        ));
                        continue;
                    }
                };
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
                match probe_stream_head(&target.conn.provider, rx).await {
                    StreamProbe::Deliver { buffered, rest } => {
                        let selection = selection_for_accepted_target(
                            &target,
                            &requested,
                            effort_policy,
                            accepted_reason(origin, &failures),
                        );
                        return Ok(RoutedStream {
                            selection,
                            events: deliver_probed(buffered, rest),
                        });
                    }
                    StreamProbe::Failover(failure) => {
                        failures.push(failure);
                        continue;
                    }
                }
            }
            ApiFormat::OpenAi => {
                // Not stripped here: `anthropic_to_openai_request` translates
                // `thinking` into `reasoning_effort` for this wire format.
                let mut upstream_body = translate::anthropic_to_openai_request(&attempt_body)?;
                apply_openai_effort(
                    &mut upstream_body,
                    &target,
                    effort_policy,
                    body.get("thinking").is_some(),
                );
                apply_max_completion_tokens(target.desc, &mut upstream_body);
                let resp = match send_upstream(ctx, &mut target, &upstream_body).await {
                    Ok(resp) => resp,
                    Err(e) => {
                        // Transport-level send failure (DNS/TLS/connect/reset):
                        // record it and rotate instead of failing the call.
                        failures.push(UpstreamAttemptFailure::transport(
                            provider,
                            format!("upstream send failed: {e}"),
                        ));
                        continue;
                    }
                };
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
                match probe_stream_head(&target.conn.provider, rx).await {
                    StreamProbe::Deliver { buffered, rest } => {
                        let selection = selection_for_accepted_target(
                            &target,
                            &requested,
                            effort_policy,
                            accepted_reason(origin, &failures),
                        );
                        return Ok(RoutedStream {
                            selection,
                            events: deliver_probed(buffered, rest),
                        });
                    }
                    StreamProbe::Failover(failure) => {
                        failures.push(failure);
                        continue;
                    }
                }
            }
        }
    }
    Err(fallback_error(&requested, &failures))
}

/// Non-streaming sibling: returns the full Anthropic message `Value`.
pub async fn anthropic_messages(ctx: &UpstreamCtx, body: Value) -> anyhow::Result<Value> {
    let requested = body["model"].as_str().unwrap_or("").to_string();
    let tool_requirements = capabilities::tool_transport_requirements_from_body(&body);
    let mut provider_order_cache = ProviderOrderCache::new();
    let targets = filter_tool_compatible(
        route_models_for_body_matching_with_cache(
            &ctx.store,
            &requested,
            None,
            anthropic_messages_target_allowed,
            &mut provider_order_cache,
            RouteOrderMode::Advance,
        )
        .await?,
        tool_requirements,
    )
    .into_iter()
    .map(|annotated| annotated.target)
    .collect::<Vec<_>>();
    if targets.is_empty() {
        anyhow::bail!("no enabled connection serves model '{requested}'");
    }

    let mut failures = Vec::new();
    let mut attempted = std::collections::HashSet::<(String, String)>::new();
    let mut queue = std::collections::VecDeque::from(targets);
    let mut continued = false;
    loop {
        let Some(mut target) = queue.pop_front() else {
            // Initial targets exhausted with only retryable failures
            // (non-retryable ones returned early above): continue down the
            // first Model Route containing the pinned (family, model) — once.
            if continued {
                break;
            }
            continued = true;
            queue.extend(
                route_continuation_targets(
                    &ctx.store,
                    &requested,
                    &attempted,
                    &mut provider_order_cache,
                    tool_requirements,
                    RouteOrderMode::Advance,
                )
                .await?
                .into_iter()
                .map(|annotated| annotated.target),
            );
            if queue.is_empty() {
                break;
            }
            continue;
        };
        attempted.insert((target.conn.id.clone(), target.upstream_model.clone()));
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
                let resp = match send_upstream(ctx, &mut target, &attempt_body).await {
                    Ok(resp) => resp,
                    Err(e) => {
                        // Transport-level send failure (DNS/TLS/connect/reset):
                        // record it and rotate instead of failing the call.
                        failures.push(UpstreamAttemptFailure::transport(
                            provider,
                            format!("upstream send failed: {e}"),
                        ));
                        continue;
                    }
                };
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
                let mut upstream_body = translate::openai_to_anthropic_request(&attempt_body)
                    .or_else(|_| translate::anthropic_to_openai_request(&attempt_body))?;
                apply_max_completion_tokens(target.desc, &mut upstream_body);
                let provider = target.conn.provider.clone();
                let resp = match send_upstream(ctx, &mut target, &upstream_body).await {
                    Ok(resp) => resp,
                    Err(e) => {
                        // Transport-level send failure (DNS/TLS/connect/reset):
                        // record it and rotate instead of failing the call.
                        failures.push(UpstreamAttemptFailure::transport(
                            provider,
                            format!("upstream send failed: {e}"),
                        ));
                        continue;
                    }
                };
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
///
/// Mirrors the OpenAI-translated pump's `saw_terminal` guard
/// (`translate::OpenAiToAnthropicStream::saw_terminal`): a 2xx stream that
/// ends without `message_stop` (or an explicit `error` event) is a truncated
/// stream, not a completed one — emit a terminal error event instead of
/// ending silently as a "(no output)" turn. Emitted before any content, the
/// error also lets `probe_stream_head` rotate to the next target.
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
    let mut saw_terminal = false;
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
            if name == "message_stop" || name == "error" {
                saw_terminal = true;
            }
            if tx.send(Ok((name, v))).await.is_err() {
                break 'pump; // consumer dropped
            }
        }
    }
    if !errored && !saw_terminal {
        let _ = tx
            .send(Ok((
                "error".to_string(),
                json!({"type": "error", "error": {"type": "api_error",
                       "message": "upstream stream ended without a terminal event"}}),
            )))
            .await;
        errored = true;
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
pub(crate) fn kiro_upstream_request(
    ctx: &UpstreamCtx,
    target: &RouteTarget,
    kiro_body: &Value,
) -> reqwest::RequestBuilder {
    let data = &target.conn.data;
    let auth_method = connections::kiro_auth_method(data);
    let url = ctx.kiro_base_override.clone().unwrap_or_else(|| {
        kiro_endpoints(&auth_method, &connections::kiro_region(data))
            .into_iter()
            .next()
            .expect("kiro_endpoints always returns at least one URL")
    });
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
    let resp = send_kiro(ctx, target, &kiro_body).await.map_err(|e| {
        UpstreamAttemptFailure::transport(
            target.conn.provider.clone(),
            format!("upstream kiro: {e}"),
        )
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

/// Divert a routed connection to its in-process WASM provider component. Mirror
/// of `kiro_stream`/`codex_stream`: returns the same
/// `Result<mpsc::Receiver<..>, UpstreamAttemptFailure>` shape so the dispatch
/// loop treats it uniformly. The component's `complete` returns ALL chunks as a
/// list; this converts them into an ordered `AnthropicEvent` stream (preserving
/// chunk order) via the same `OpenAiToAnthropicStream` the HTTP pumps use, so
/// the synthesized message_start/content_block_delta/message_stop shape matches
/// exactly. A guest `provider-error` or a host-side trap/timeout becomes a
/// route-scoped `UpstreamAttemptFailure` (never a panic or a daemon crash).
async fn wasm_provider_stream(
    ctx: &UpstreamCtx,
    target: &mut RouteTarget,
    body: &Value,
    transport: Arc<dyn crate::plugins::wasm_provider::WasmProviderRuntime>,
    started: i64,
) -> Result<mpsc::Receiver<anyhow::Result<AnthropicEvent>>, UpstreamAttemptFailure> {
    let provider = target.conn.provider.clone();
    let model = target.upstream_model.clone();
    let request = crate::plugins::wasm_provider::WasmCompletionRequest {
        model: model.clone(),
        prompt: flatten_anthropic_prompt(body),
        max_tokens: body["max_tokens"]
            .as_u64()
            .and_then(|v| u32::try_from(v).ok()),
        temperature: body["temperature"].as_f64().map(|v| v as f32),
    };
    // The whole completion is produced up front. A trap/provider-error surfaces
    // here (before any events are yielded) as a route-scoped failure, so the
    // dispatch loop can fail over to the next target.
    let chunks = transport.complete(request).await.map_err(|message| {
        UpstreamAttemptFailure::upstream(provider.clone(), format!("wasm provider: {message}"))
    })?;
    let (tx, rx) = mpsc::channel::<anyhow::Result<AnthropicEvent>>(64);
    let store = ctx.store.clone();
    let conn_id = target.conn.id.clone();
    tokio::spawn(async move {
        pump_wasm_provider(chunks, model, tx, store, conn_id, provider, started).await;
    });
    Ok(rx)
}

/// Convert an ordered list of WASM provider completion chunks into Anthropic SSE
/// events, preserving chunk order. Each chunk is fed to the shared
/// `OpenAiToAnthropicStream` as a synthetic OpenAI streaming chunk, so the event
/// shape is identical to the HTTP pumps'. A leading empty feed guarantees a
/// well-formed `message_start` even for an empty completion.
async fn pump_wasm_provider(
    chunks: Vec<crate::plugins::wasm_provider::WasmCompletionChunk>,
    model: String,
    tx: mpsc::Sender<anyhow::Result<AnthropicEvent>>,
    store: Arc<Store>,
    conn_id: String,
    provider: String,
    started: i64,
) {
    let mut tr = translate::OpenAiToAnthropicStream::new(&model);
    let mut input = 0i64;
    let mut output = 0i64;
    // Establish message_start up front (empty content emits no content block),
    // so even a zero-chunk completion produces a valid stream head.
    for event in tr.feed(&json!({"choices": [{"delta": {"content": ""}, "finish_reason": null}]})) {
        if tx.send(Ok(event)).await.is_err() {
            return; // consumer dropped
        }
    }
    'pump: for chunk in chunks {
        let mut oai =
            json!({"choices": [{"delta": {"content": chunk.text}, "finish_reason": null}]});
        if chunk.finished {
            oai["choices"][0]["finish_reason"] = json!("stop");
        }
        if let Some(usage) = &chunk.usage {
            input = usage.input as i64;
            output = usage.output as i64;
            oai["usage"] = json!({"prompt_tokens": usage.input, "completion_tokens": usage.output});
        }
        for event in tr.feed(&oai) {
            if tx.send(Ok(event)).await.is_err() {
                break 'pump; // consumer dropped
            }
        }
    }
    // A completion whose chunks NEVER set `finished` (guest-controlled, nothing
    // enforces it) ended without a terminal event — that is a truncated
    // response, not a completed one, so emit an `error_frame` and record the
    // attempt as failed rather than synthesizing a `message_stop` with a
    // fabricated `stop_reason` and logging a `200`. This mirrors the OpenAI /
    // kiro / codex pumps' `saw_terminal()` guard (see
    // `OpenAiToAnthropicStream`'s doc); like them it is post-hoc detection —
    // `complete()` has already returned — so it cannot do live failover, but it
    // must stop reporting a truncated wasm-provider response as success.
    // A completion whose chunks NEVER set `finished` (guest-controlled, nothing
    // enforces it) ended without a terminal event — that is a truncated
    // response, not a completed one, so emit an `error_frame` and record the
    // attempt as failed rather than synthesizing a `message_stop` with a
    // fabricated `stop_reason` and logging a `200`. This mirrors the OpenAI /
    // kiro / codex pumps' `saw_terminal()` guard (see
    // `OpenAiToAnthropicStream`'s doc); like them it is post-hoc detection —
    // `complete()` has already returned — so it cannot do live failover, but it
    // must stop reporting a truncated wasm-provider response as success.
    let mut errored = false;
    if tr.saw_terminal() {
        for event in tr.finish() {
            let _ = tx.send(Ok(event)).await;
        }
    } else {
        for event in tr.error_frame("wasm provider completion ended without a finished chunk") {
            let _ = tx.send(Ok(event)).await;
        }
        errored = true;
    }
    crate::llm_router::usage::record(
        &store,
        &conn_id,
        &provider,
        &model,
        "native",
        crate::llm_router::usage::Usage { input, output },
        if errored { 502 } else { 200 },
        started,
        errored.then(|| "completion ended without a finished chunk".to_string()),
    );
}

/// Flatten an Anthropic-Messages body into the single `prompt` string the
/// generic WASM `provider` ABI takes: the system text followed by each message's
/// text content, in order. A generic provider gets a flattened prompt rather
/// than a role-structured transcript — lossy but sufficient for the ABI's
/// deliberately minimal shape.
fn flatten_anthropic_prompt(body: &Value) -> String {
    fn push_content(content: &Value, parts: &mut Vec<String>) {
        match content {
            Value::String(text) => parts.push(text.clone()),
            Value::Array(blocks) => {
                for block in blocks {
                    if let Some(text) = block["text"].as_str() {
                        parts.push(text.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    let mut parts = Vec::new();
    push_content(&body["system"], &mut parts);
    if let Some(messages) = body["messages"].as_array() {
        for message in messages {
            push_content(&message["content"], &mut parts);
        }
    }
    parts.join("\n")
}

/// Start a Codex stream for the native path: Anthropic body -> OpenAI chat ->
/// Responses request -> Codex-normalized -> POST (via the existing openai-oauth
/// upstream branch), then a pump that yields Anthropic events.
async fn codex_stream(
    ctx: &UpstreamCtx,
    target: &mut RouteTarget,
    body: &Value,
    effort_policy: &model_effort::TurnEffortPolicy,
    started: i64,
) -> Result<mpsc::Receiver<anyhow::Result<AnthropicEvent>>, UpstreamAttemptFailure> {
    // Clone the provider up front rather than borrowing `target` inside the
    // closure — a reborrow through `target` (a `&mut`) here would keep it
    // immutably borrowed across the `send_upstream(ctx, target, ...)` call
    // below, which needs `&mut target`.
    let provider = target.conn.provider.clone();
    let fail = |e: String| UpstreamAttemptFailure::transport(provider.clone(), e);
    let chat = translate::anthropic_to_openai_request(body).map_err(|e| fail(e.to_string()))?;
    let mut responses = crate::llm_router::codex::openai_chat_to_responses_request(&chat);
    let preference_key = model_effort::ModelPreferenceKey {
        family: target.desc.family.to_string(),
        model: target.upstream_model.clone(),
    };
    let surface = model_effort::ExecutionSurfaceKey {
        provider_id: target.conn.provider.clone(),
        connection_id: Some(target.conn.id.clone()),
        model: target.upstream_model.clone(),
    };
    let effort = resolve_target_effort(
        effort_policy,
        target.route_target_key.as_ref(),
        &preference_key,
        &surface,
    );
    if let Some(effort) = effort.value.as_deref() {
        crate::llm_router::codex::apply_native_reasoning_effort(&mut responses, effort);
    }
    crate::llm_router::codex::normalize_codex_responses_body(
        &mut responses,
        &target.upstream_model,
        None,
        None,
    );
    let resp = send_upstream(ctx, target, &responses)
        .await
        .map_err(|e| fail(e.to_string()))?;
    if !resp.status().is_success() {
        return Err(upstream_status_failure(target.conn.provider.clone(), resp).await);
    }
    let (tx, rx) = mpsc::channel::<anyhow::Result<AnthropicEvent>>(64);
    let store = ctx.store.clone();
    let conn_id = target.conn.id.clone();
    let provider = target.conn.provider.clone();
    let model = target.upstream_model.clone();
    tokio::spawn(async move {
        pump_codex(resp, model, tx, store, conn_id, provider, started).await;
    });
    Ok(rx)
}

/// Pump a Codex Responses SSE response into Anthropic events: decode with
/// `ResponsesToOpenAiStream`, translate via `OpenAiToAnthropicStream`. A
/// `response.failed`/`error` event decodes to a bare `{"error":{"message":
/// ...}}` element (see `ResponsesToOpenAiStream::feed`) that
/// `OpenAiToAnthropicStream::feed` doesn't understand and would silently
/// drop — it's checked for explicitly and turned into an Anthropic error
/// frame before it ever reaches `tr.feed`.
async fn pump_codex(
    resp: reqwest::Response,
    model: String,
    tx: mpsc::Sender<anyhow::Result<AnthropicEvent>>,
    store: Arc<Store>,
    conn_id: String,
    provider: String,
    started: i64,
) {
    use crate::llm_router::sse::SseParser;
    use futures::StreamExt;
    let mut parser = SseParser::default();
    let mut dec = crate::llm_router::codex::ResponsesToOpenAiStream::new(&model);
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
            let Ok(data) = serde_json::from_str::<Value>(&ev.data) else {
                continue;
            };
            let name = ev.event.clone().unwrap_or_default();
            for oai in dec.feed(&name, &data) {
                if let Some(err) = oai
                    .get("error")
                    .and_then(|e| e.get("message"))
                    .and_then(|m| m.as_str())
                {
                    for (n, d) in tr.error_frame(err) {
                        let _ = tx.send(Ok((n, d))).await;
                    }
                    errored = true;
                    break 'pump;
                }
                for (n, d) in tr.feed(&oai) {
                    if tx.send(Ok((n, d))).await.is_err() {
                        break 'pump;
                    }
                }
            }
        }
    }
    if !errored {
        if !dec.saw_terminal() {
            for oai in dec.finish() {
                for (n, d) in tr.feed(&oai) {
                    let _ = tx.send(Ok((n, d))).await;
                }
            }
        }
        for (n, d) in tr.finish() {
            let _ = tx.send(Ok((n, d))).await;
        }
    }
    let (input, output) = dec.usage();
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

    fn single_option_policy(target: &RouteTarget, value: &str) -> model_effort::TurnEffortPolicy {
        let surface = model_effort::ExecutionSurfaceKey {
            provider_id: target.conn.provider.clone(),
            connection_id: Some(target.conn.id.clone()),
            model: target.upstream_model.clone(),
        };
        model_effort::TurnEffortPolicy {
            requested_model: format!("{}/{}", target.desc.family, target.upstream_model),
            caller_override: None,
            route_targets: Default::default(),
            configured: Default::default(),
            surfaces: std::collections::HashMap::from([(
                surface.clone(),
                model_effort::ExecutionModelEffortCapabilities {
                    surface,
                    model_display_name: target.upstream_model.clone(),
                    supported: vec![model_effort::ReasoningEffortOption {
                        value: value.into(),
                        label: value.into(),
                        description: None,
                    }],
                    provider_default: None,
                },
            )]),
        }
    }

    #[test]
    fn kiro_protocol_strips_all_client_effort_fields() {
        let mut body = json!({
            "output_config": {"effort": "low", "other": true},
            "reasoning_effort": "medium",
            "reasoning": {"effort": "high", "summary": "detailed"}
        });

        strip_kiro_effort(&mut body);

        assert!(body["output_config"].get("effort").is_none());
        assert_eq!(body["output_config"]["other"], true);
        assert!(body.get("reasoning_effort").is_none());
        assert!(body["reasoning"].get("effort").is_none());
        assert_eq!(body["reasoning"]["summary"], "detailed");
    }

    #[test]
    fn single_option_without_advertised_default_is_omitted_from_wire() {
        let anthropic = RouteTarget {
            conn: mk_conn("a1", "anthropic", "api_key", ConnectionData::default()),
            desc: registry::descriptor("anthropic").unwrap(),
            upstream_model: "claude-one".into(),
            route_target_key: None,
        };
        let policy = single_option_policy(&anthropic, "focused");
        let mut anthropic_body = json!({"messages": []});
        apply_anthropic_effort(&mut anthropic_body, &anthropic, &policy);
        assert!(anthropic_body
            .get("output_config")
            .and_then(|output| output.get("effort"))
            .is_none());

        let openai = RouteTarget {
            conn: mk_conn("o1", "openai", "api_key", ConnectionData::default()),
            desc: registry::descriptor("openai").unwrap(),
            upstream_model: "gpt-one".into(),
            route_target_key: None,
        };
        let policy = single_option_policy(&openai, "focused");
        let mut chat_body = json!({"messages": []});
        apply_openai_effort(&mut chat_body, &openai, &policy, false);
        assert!(chat_body.get("reasoning_effort").is_none());

        let codex = RouteTarget {
            conn: mk_conn("c1", "openai-oauth", "oauth", ConnectionData::default()),
            desc: registry::descriptor("openai-oauth").unwrap(),
            upstream_model: "gpt-codex".into(),
            route_target_key: None,
        };
        let policy = single_option_policy(&codex, "ultra");
        assert!(target_effort(&codex, &policy).value.is_none());
    }

    #[tokio::test]
    async fn codex_stream_sends_native_policy_effort_and_omits_unsupported_effort() {
        use axum::{extract::State, routing::post, Json, Router};
        use std::sync::Mutex as StdMutex;

        async fn capture(
            State(captured): State<Arc<StdMutex<Vec<Value>>>>,
            Json(body): Json<Value>,
        ) -> ([(&'static str, &'static str); 1], &'static str) {
            captured.lock().unwrap().push(body);
            (
                [("content-type", "text/event-stream")],
                "event: response.output_text.delta\ndata: {\"delta\":\"ok\"}\n\nevent: response.completed\ndata: {\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
            )
        }

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let app = Router::new()
            .route("/responses", post(capture))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "codex-wire",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("test-token".into()),
                    expires_at: Some(crate::paths::now_ms() + 100 * 24 * 60 * 60 * 1_000),
                    last_refresh_at: Some(crate::paths::now_ms()),
                    needs_relogin: Some(false),
                    base_url_override: Some(format!("http://127.0.0.1:{port}")),
                    models_override: Some(vec!["gpt-wire".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        let surface = model_effort::ExecutionSurfaceKey {
            provider_id: "openai-oauth".into(),
            connection_id: Some("codex-wire".into()),
            model: "gpt-wire".into(),
        };
        let policy = Arc::new(model_effort::TurnEffortPolicy {
            requested_model: "openai/gpt-wire".into(),
            caller_override: Some("ultra".into()),
            route_targets: Default::default(),
            configured: Default::default(),
            surfaces: std::collections::HashMap::from([(
                surface.clone(),
                model_effort::ExecutionModelEffortCapabilities {
                    surface,
                    model_display_name: "GPT Wire".into(),
                    supported: vec![model_effort::ReasoningEffortOption {
                        value: "ultra".into(),
                        label: "Ultra".into(),
                        description: None,
                    }],
                    provider_default: None,
                },
            )]),
        });
        let body = || {
            json!({
                "model": "openai/gpt-wire",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true
            })
        };

        let rx = anthropic_messages_stream(&ctx, body(), &policy)
            .await
            .unwrap();
        let _ = collect_stream(rx.events).await;

        let unsupported = Arc::new(model_effort::TurnEffortPolicy {
            requested_model: "openai/gpt-wire".into(),
            caller_override: Some("ultra".into()),
            route_targets: Default::default(),
            configured: Default::default(),
            surfaces: Default::default(),
        });
        let rx = anthropic_messages_stream(&ctx, body(), &unsupported)
            .await
            .unwrap();
        let _ = collect_stream(rx.events).await;

        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 2);
        assert_eq!(captured[0]["reasoning"]["effort"], "max");
        assert!(captured[1].get("reasoning").is_none());
    }

    #[tokio::test]
    async fn named_route_target_effort_overrides_caller_and_uses_model_then_provider_defaults() {
        use axum::{extract::State, routing::post, Json, Router};
        use std::sync::Mutex as StdMutex;

        async fn capture(
            State(captured): State<Arc<StdMutex<Vec<Value>>>>,
            Json(body): Json<Value>,
        ) -> ([(&'static str, &'static str); 1], &'static str) {
            captured.lock().unwrap().push(body);
            (
                [("content-type", "text/event-stream")],
                "event: response.output_text.delta\ndata: {\"delta\":\"ok\"}\n\nevent: response.completed\ndata: {\"response\":{\"usage\":{\"input_tokens\":1,\"output_tokens\":1}}}\n\n",
            )
        }

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let app = Router::new()
            .route("/responses", post(capture))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "route-wire",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("test-token".into()),
                    expires_at: Some(crate::paths::now_ms() + 100 * 24 * 60 * 60 * 1_000),
                    last_refresh_at: Some(crate::paths::now_ms()),
                    needs_relogin: Some(false),
                    base_url_override: Some(format!("http://127.0.0.1:{port}")),
                    models_override: Some(vec!["gpt-5.5".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "route-effort".into(),
                name: "smart-wire".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![routes::ModelRouteTarget {
                    provider: "openai".into(),
                    model: "gpt-5.5".into(),
                    effort: Some("high".into()),
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let surface = model_effort::ExecutionSurfaceKey {
            provider_id: "openai-oauth".into(),
            connection_id: Some("route-wire".into()),
            model: "gpt-5.5".into(),
        };
        let preference = model_effort::ModelPreferenceKey {
            family: "openai".into(),
            model: "gpt-5.5".into(),
        };
        let capability = model_effort::ExecutionModelEffortCapabilities {
            surface: surface.clone(),
            model_display_name: "GPT Route Wire".into(),
            supported: ["low", "medium", "high"]
                .into_iter()
                .map(|value| model_effort::ReasoningEffortOption {
                    value: value.into(),
                    label: value.into(),
                    description: None,
                })
                .collect(),
            provider_default: Some("high".into()),
        };
        let request = || {
            json!({
                "model": "smart-wire",
                "messages": [{"role": "user", "content": "hi"}],
                "stream": true
            })
        };
        let mut base_policy = model_effort::build_utility_effort_policy(&ctx.store, "smart-wire")
            .await
            .unwrap();
        base_policy.caller_override = Some("low".into());
        base_policy.configured = std::collections::HashMap::from([(preference, "medium".into())]);
        base_policy.surfaces = std::collections::HashMap::from([(surface, capability)]);

        let first = anthropic_messages_stream(&ctx, request(), &Arc::new(base_policy.clone()))
            .await
            .unwrap();
        let _ = collect_stream(first.events).await;

        let mut model_default = base_policy;
        model_default.route_targets.clear();
        let second = anthropic_messages_stream(&ctx, request(), &Arc::new(model_default.clone()))
            .await
            .unwrap();
        let _ = collect_stream(second.events).await;

        model_default.configured.clear();
        let third = anthropic_messages_stream(&ctx, request(), &Arc::new(model_default))
            .await
            .unwrap();
        let _ = collect_stream(third.events).await;

        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 3);
        assert_eq!(captured[0]["reasoning"]["effort"], "high");
        assert_eq!(captured[1]["reasoning"]["effort"], "medium");
        assert_eq!(captured[2]["reasoning"]["effort"], "high");
    }

    async fn test_ctx() -> UpstreamCtx {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        UpstreamCtx::new(store)
    }

    /// Leak + register a minimal `&'static ProviderDescriptor` for a WASM
    /// provider `id` (its own family head, so routes accept it), mirroring
    /// `llm_router::custom::register`. Base URL is a dummy — the router diverts a
    /// WASM provider to its in-process component before any HTTP is attempted.
    fn register_wasm_descriptor(id: &'static str) {
        let desc: &'static ProviderDescriptor = Box::leak(Box::new(ProviderDescriptor {
            id,
            name: "WASM Fixture Provider",
            family: id,
            color: "#8B8B8B",
            initial: "W",
            category: registry::ProviderCategory::ApiKey,
            format: ApiFormat::OpenAi,
            tool_transport: registry::ProviderToolTransport::for_format(ApiFormat::OpenAi),
            base_url: Some("http://127.0.0.1"),
            auth: AuthScheme::Bearer,
            models: &["fixture-model"],
            requires_base_url: false,
            oauth: None,
            no_auth: false,
            device_flow: None,
            free_tier: false,
            risk_notice: false,
            chat_path: None,
            has_models_endpoint: false,
            uses_max_completion_tokens: false,
            device_grant: None,
        }));
        registry::register_custom_descriptor(desc);
    }

    fn empty_policy(requested_model: &str) -> Arc<model_effort::TurnEffortPolicy> {
        Arc::new(model_effort::TurnEffortPolicy {
            requested_model: requested_model.to_string(),
            caller_override: None,
            route_targets: Default::default(),
            configured: Default::default(),
            surfaces: Default::default(),
        })
    }

    /// Step-1: generic LLM routing diverts a connection backed by an installed
    /// WASM provider bundle to its in-process component, preserves completion
    /// chunk ORDER in the emitted Anthropic stream, and converts a component
    /// trap into a route-scoped error (never a panic).
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn routing_diverts_to_a_wasm_provider_preserving_chunk_order_and_isolating_a_trap() {
        crate::plugins::build_fixture_components_once();
        const PROVIDER_ID: &str = "wasm-router-fixture";
        let ctx = test_ctx().await;

        register_wasm_descriptor(PROVIDER_ID);
        crate::llm_router::installed::install_provider(&ctx.store, PROVIDER_ID)
            .await
            .unwrap();
        let (transport, _tmp) = crate::plugins::wasm_provider::build_test_transport(
            crate::plugins::wasm_provider::provider_fixture_artifact(),
            PROVIDER_ID,
            std::time::Duration::from_secs(10),
        )
        .await;
        crate::plugins::wasm_provider::register_wasm_provider(transport);
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "wasm-conn",
                PROVIDER_ID,
                "api_key",
                ConnectionData {
                    api_key: Some("unused".into()),
                    models_override: Some(vec!["fixture-model".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        // Happy path: the two-chunk completion streams IN ORDER.
        let requested = format!("{PROVIDER_ID}/fixture-model");
        let routed = anthropic_messages_stream(
            &ctx,
            json!({
                "model": requested,
                "messages": [{"role": "user", "content": "say hello"}]
            }),
            &empty_policy(&requested),
        )
        .await
        .expect("routing to a wasm provider must succeed");
        let events = collect_stream(routed.events).await;
        let text: String = events
            .iter()
            .filter(|(name, _)| name == "content_block_delta")
            .filter_map(|(_, data)| data["delta"]["text"].as_str())
            .collect();
        assert_eq!(
            text, "Hello, world!",
            "the router must preserve wasm provider chunk order"
        );
        assert!(
            events.iter().any(|(name, _)| name == "message_start"),
            "the synthesized stream must open with message_start"
        );
        assert!(
            events.iter().any(|(name, _)| name == "message_stop"),
            "the synthesized stream must close with message_stop"
        );

        // A trapping completion becomes a route-scoped error, not a panic.
        // (`RoutedStream` is not `Debug`, so match instead of `expect_err`.)
        let error = match anthropic_messages_stream(
            &ctx,
            json!({
                "model": requested,
                "messages": [{"role": "user", "content": "please boom now"}]
            }),
            &empty_policy(&requested),
        )
        .await
        {
            Ok(_) => panic!("a trapping wasm completion must surface as a route-scoped error"),
            Err(error) => error,
        };
        assert!(
            error.to_string().contains("wasm provider"),
            "the route-scoped error must name the wasm provider: {error}"
        );

        crate::plugins::wasm_provider::unregister_wasm_provider(PROVIDER_ID);
        registry::unregister_custom_descriptor(PROVIDER_ID);
    }

    /// Regression: a WASM provider completion whose chunks NEVER set `finished`
    /// is a truncated response, so the router must emit an `error` frame and NOT
    /// synthesize a completed `message_stop` (which would fabricate a success and
    /// silently defeat the router's truncated-stream failover for wasm
    /// providers). Mirrors the OpenAI/kiro/codex pumps' `saw_terminal()` guard.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn routing_reports_a_wasm_provider_completion_with_no_finished_chunk_as_truncated() {
        crate::plugins::build_fixture_components_once();
        const PROVIDER_ID: &str = "wasm-router-noterm";
        let ctx = test_ctx().await;

        register_wasm_descriptor(PROVIDER_ID);
        crate::llm_router::installed::install_provider(&ctx.store, PROVIDER_ID)
            .await
            .unwrap();
        let (transport, _tmp) = crate::plugins::wasm_provider::build_test_transport(
            crate::plugins::wasm_provider::provider_fixture_artifact(),
            PROVIDER_ID,
            std::time::Duration::from_secs(10),
        )
        .await;
        crate::plugins::wasm_provider::register_wasm_provider(transport);
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "wasm-conn-noterm",
                PROVIDER_ID,
                "api_key",
                ConnectionData {
                    api_key: Some("unused".into()),
                    models_override: Some(vec!["fixture-model".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let requested = format!("{PROVIDER_ID}/fixture-model");
        let routed = anthropic_messages_stream(
            &ctx,
            json!({
                "model": requested,
                "messages": [{"role": "user", "content": "give me an unterminated answer"}]
            }),
            &empty_policy(&requested),
        )
        .await
        .expect("routing to the wasm provider must still deliver a (truncated) stream");
        let events = collect_stream(routed.events).await;

        // The truncated completion ends with an `error` frame, NOT a fabricated
        // `message_stop`. On the pre-fix code the pump called `finish()`
        // unconditionally, so `message_stop` was present and the `error` frame
        // absent — this assertion is the RED→GREEN.
        let error_message = events
            .iter()
            .find(|(name, _)| name == "error")
            .map(|(_, data)| {
                data["error"]["message"]
                    .as_str()
                    .unwrap_or_default()
                    .to_string()
            })
            .expect("a truncated wasm completion must emit an error frame");
        assert!(
            error_message.contains("without a finished chunk"),
            "unexpected error frame message: {error_message}"
        );
        assert!(
            !events.iter().any(|(name, _)| name == "message_stop"),
            "a truncated completion must NOT synthesize a completed message_stop"
        );

        crate::plugins::wasm_provider::unregister_wasm_provider(PROVIDER_ID);
        registry::unregister_custom_descriptor(PROVIDER_ID);
    }

    #[tokio::test]
    async fn native_kiro_stream_strips_all_client_effort_fields_before_translation() {
        use axum::body::Body;
        use axum::http::header;
        use axum::response::Response;
        use axum::{extract::State, routing::post, Json, Router};
        use std::sync::Mutex as StdMutex;

        fn frame(event_type: &str, payload: &str) -> Vec<u8> {
            let name = ":event-type";
            let mut headers = Vec::new();
            headers.push(name.len() as u8);
            headers.extend_from_slice(name.as_bytes());
            headers.push(7);
            headers.extend_from_slice(&(event_type.len() as u16).to_be_bytes());
            headers.extend_from_slice(event_type.as_bytes());
            let payload = payload.as_bytes();
            let total = 12 + headers.len() + payload.len() + 4;
            let mut out = Vec::with_capacity(total);
            out.extend_from_slice(&(total as u32).to_be_bytes());
            out.extend_from_slice(&(headers.len() as u32).to_be_bytes());
            out.extend_from_slice(&[0; 4]);
            out.extend_from_slice(&headers);
            out.extend_from_slice(payload);
            out.extend_from_slice(&[0; 4]);
            out
        }

        async fn capture(
            State(captured): State<Arc<StdMutex<Vec<Value>>>>,
            Json(body): Json<Value>,
        ) -> Response {
            captured.lock().unwrap().push(body);
            let mut response = frame("assistantResponseEvent", r#"{"content":"ok"}"#);
            response.extend(frame("messageStopEvent", "{}"));
            Response::builder()
                .status(200)
                .header(header::CONTENT_TYPE, "application/vnd.amazon.eventstream")
                .body(Body::from(response))
                .unwrap()
        }

        let captured = Arc::new(StdMutex::new(Vec::new()));
        let app = Router::new()
            .route("/generateAssistantResponse", post(capture))
            .with_state(captured.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let mut ctx = test_ctx().await;
        ctx.kiro_base_override = Some(format!("http://127.0.0.1:{port}/generateAssistantResponse"));
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "kiro-native",
                "kiro",
                "oauth",
                ConnectionData {
                    access_token: Some("kiro-token".into()),
                    expires_at: Some(crate::paths::now_ms() + 86_400_000),
                    last_refresh_at: Some(crate::paths::now_ms()),
                    models_override: Some(vec!["claude-sonnet-4.5".into()]),
                    provider_specific: Some(json!({"authMethod": "builder-id"})),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let routed = anthropic_messages_stream(
            &ctx,
            json!({
                "model": "kiro/claude-sonnet-4.5",
                "messages": [{"role": "user", "content": "hi"}],
                "output_config": {"effort": "low"},
                "reasoning_effort": "medium",
                "reasoning": {"effort": "high"}
            }),
            utility_policy(&ctx, "kiro/claude-sonnet-4.5")
                .await
                .as_ref(),
        )
        .await
        .unwrap();
        let _ = collect_stream(routed.events).await;

        let captured = captured.lock().unwrap();
        assert_eq!(captured.len(), 1);
        assert!(captured[0]["inferenceConfig"].get("maxTokens").is_some());
        assert!(captured[0].pointer("/output_config/effort").is_none());
        assert!(captured[0].get("reasoning_effort").is_none());
        assert!(captured[0].pointer("/reasoning/effort").is_none());
    }

    async fn utility_policy(ctx: &UpstreamCtx, model: &str) -> Arc<model_effort::TurnEffortPolicy> {
        Arc::new(
            model_effort::build_utility_effort_policy(&ctx.store, model)
                .await
                .unwrap(),
        )
    }

    /// Drain an `anthropic_messages_stream` receiver, panicking on transport
    /// `Err` items (none of these tests expect one).
    async fn collect_stream(
        mut rx: mpsc::Receiver<anyhow::Result<AnthropicEvent>>,
    ) -> Vec<AnthropicEvent> {
        let mut out = Vec::new();
        while let Some(item) = rx.recv().await {
            out.push(item.expect("stream must not carry transport Err items in this test"));
        }
        out
    }

    async fn provenance_test_server(first: &'static str, second: &'static str) -> u16 {
        use axum::{routing::post, Router};
        let app = Router::new()
            .route(
                "/first/messages",
                post(move || async move { ([("content-type", "text/event-stream")], first) }),
            )
            .route(
                "/second/messages",
                post(move || async move { ([("content-type", "text/event-stream")], second) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        port
    }

    async fn add_provenance_account(
        ctx: &UpstreamCtx,
        id: &str,
        label: &str,
        port: u16,
        path: &str,
        model: &str,
    ) {
        let mut conn = mk_conn(
            id,
            "anthropic",
            "api_key",
            ConnectionData {
                api_key: Some(format!("sk-{id}")),
                base_url_override: Some(format!("http://127.0.0.1:{port}/{path}")),
                models_override: Some(vec![model.into()]),
                ..Default::default()
            },
        );
        conn.label = label.into();
        connections::add_connection(&ctx.store, conn).await.unwrap();
    }

    #[tokio::test]
    async fn route_selection_reports_exact_successful_target() {
        let port = provenance_test_server(SSE_OK_STREAM, SSE_OK_STREAM).await;
        let ctx = test_ctx().await;
        add_provenance_account(&ctx, "only", "Only Claude", port, "first", "claude-t").await;
        let routed = anthropic_messages_stream(
            &ctx,
            json!({"model":"anthropic/claude-t","messages":[]}),
            utility_policy(&ctx, "anthropic/claude-t").await.as_ref(),
        )
        .await
        .unwrap();
        assert_eq!(routed.selection.requested_model, "anthropic/claude-t");
        assert_eq!(routed.selection.resolved_provider_id, "anthropic");
        assert_eq!(routed.selection.resolved_family, "anthropic");
        assert_eq!(routed.selection.resolved_model, "claude-t");
        assert_eq!(routed.selection.connection_id, "only");
        assert_eq!(routed.selection.connection_label, "Only Claude");
        assert_eq!(
            routed.selection.reason,
            crate::llm_router::provenance::RouteSelectionReason::Initial
        );
        assert_eq!(stream_text(&collect_stream(routed.events).await), "rotated");
    }

    #[tokio::test]
    async fn route_selection_reports_named_and_account_round_robin() {
        let port = provenance_test_server(SSE_OK_STREAM, SSE_OK_STREAM).await;
        let ctx = test_ctx().await;
        add_provenance_account(&ctx, "first", "First", port, "first", "claude-t").await;
        add_provenance_account(&ctx, "second", "Second", port, "second", "claude-t").await;
        routes::save_provider_account_route(
            &ctx.store,
            "anthropic",
            routes::ModelRouteStrategy::RoundRobin,
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "rr-route".into(),
                name: "free".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::RoundRobin,
                targets: vec![
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-t".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-t".into(),
                        effort: None,
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let routed = anthropic_messages_stream(
            &ctx,
            json!({"model":"free","messages":[]}),
            utility_policy(&ctx, "free").await.as_ref(),
        )
        .await
        .unwrap();
        assert_eq!(
            routed.selection.reason,
            crate::llm_router::provenance::RouteSelectionReason::RoundRobin
        );
        assert!(matches!(
            routed.selection.connection_id.as_str(),
            "first" | "second"
        ));
    }

    #[tokio::test]
    async fn route_selection_duplicate_targets_advance_each_round_robin_cursor_once() {
        let port = provenance_test_server(SSE_OK_STREAM, SSE_OK_STREAM).await;
        let ctx = test_ctx().await;
        add_provenance_account(&ctx, "first", "First", port, "first", "claude-t").await;
        add_provenance_account(&ctx, "second", "Second", port, "second", "claude-t").await;
        routes::save_provider_account_route(
            &ctx.store,
            "anthropic",
            routes::ModelRouteStrategy::RoundRobin,
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "duplicate-rr".into(),
                name: "duplicate-smart".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::RoundRobin,
                targets: vec![
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-t".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-t".into(),
                        effort: None,
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let policy = utility_policy(&ctx, "duplicate-smart").await;
        let request = || json!({"model":"duplicate-smart","messages":[]});

        let first = anthropic_messages_stream(&ctx, request(), &policy)
            .await
            .unwrap();
        assert_eq!(first.selection.connection_id, "first");
        assert_eq!(
            ctx.store
                .get_setting("llm_provider_account_round_robin_cursor.anthropic.claude-t")
                .await
                .unwrap()
                .as_deref(),
            Some("1")
        );
        assert_eq!(
            ctx.store
                .get_setting("llm_model_route_round_robin_cursor.duplicate-rr")
                .await
                .unwrap()
                .as_deref(),
            Some("1")
        );

        let second = anthropic_messages_stream(&ctx, request(), &policy)
            .await
            .unwrap();
        assert_eq!(second.selection.connection_id, "second");
        assert_eq!(
            ctx.store
                .get_setting("llm_provider_account_round_robin_cursor.anthropic.claude-t")
                .await
                .unwrap()
                .as_deref(),
            Some("0")
        );
        assert_eq!(
            ctx.store
                .get_setting("llm_model_route_round_robin_cursor.duplicate-rr")
                .await
                .unwrap()
                .as_deref(),
            Some("0")
        );
    }

    #[test]
    fn route_selection_failover_classifies_auth_quota_rate_transport_and_unavailable() {
        let cases = [
            (
                UpstreamAttemptFailure::http("p", 401, "auth-secret-sentinel"),
                RouteFailureCategory::Authentication,
            ),
            (
                UpstreamAttemptFailure::http("p", 429, "quota-secret-sentinel exceeded"),
                RouteFailureCategory::Quota,
            ),
            (
                UpstreamAttemptFailure::http("p", 429, "rate-secret-sentinel limit"),
                RouteFailureCategory::RateLimit,
            ),
            (
                UpstreamAttemptFailure::transport("p", "transport-secret-sentinel"),
                RouteFailureCategory::Transport,
            ),
            (
                UpstreamAttemptFailure::http("p", 503, "unavailable-secret-sentinel"),
                RouteFailureCategory::Unavailable,
            ),
        ];
        for (failure, expected) in cases {
            assert_eq!(failure.category, expected);
            let reason =
                crate::llm_router::provenance::RouteSelectionReason::Failover(failure.category);
            assert_eq!(
                reason,
                crate::llm_router::provenance::RouteSelectionReason::Failover(expected)
            );
            assert!(!format!("{reason:?}").contains("secret-sentinel"));
        }
    }

    #[tokio::test]
    async fn route_selection_never_reports_failed_candidate() {
        let port = provenance_test_server(SSE_ERROR_BEFORE_CONTENT, SSE_OK_STREAM).await;
        let ctx = test_ctx().await;
        add_provenance_account(
            &ctx,
            "failed",
            "Failed secret-sentinel",
            port,
            "first",
            "claude-t",
        )
        .await;
        add_provenance_account(&ctx, "accepted", "Accepted", port, "second", "claude-t").await;
        let routed = anthropic_messages_stream(
            &ctx,
            json!({"model":"anthropic/claude-t","messages":[]}),
            utility_policy(&ctx, "anthropic/claude-t").await.as_ref(),
        )
        .await
        .unwrap();
        assert_eq!(routed.selection.connection_id, "accepted");
        assert_eq!(
            routed.selection.reason,
            crate::llm_router::provenance::RouteSelectionReason::Failover(
                RouteFailureCategory::Unavailable
            )
        );
        assert!(!format!("{:?}", routed.selection).contains("secret-sentinel"));
    }

    #[tokio::test]
    async fn route_selection_exhaustion_returns_no_stream() {
        let port = provenance_test_server(SSE_ERROR_BEFORE_CONTENT, SSE_ERROR_BEFORE_CONTENT).await;
        let ctx = test_ctx().await;
        add_provenance_account(&ctx, "first", "First", port, "first", "claude-t").await;
        add_provenance_account(&ctx, "second", "Second", port, "second", "claude-t").await;
        assert!(anthropic_messages_stream(
            &ctx,
            json!({"model":"anthropic/claude-t","messages":[]}),
            utility_policy(&ctx, "anthropic/claude-t").await.as_ref()
        )
        .await
        .is_err());
    }

    #[tokio::test]
    async fn route_selection_midstream_error_keeps_accepted_selection() {
        let port = provenance_test_server(SSE_CONTENT_THEN_ERROR, SSE_OK_STREAM).await;
        let ctx = test_ctx().await;
        add_provenance_account(&ctx, "accepted", "Accepted", port, "first", "claude-t").await;
        add_provenance_account(&ctx, "unused", "Unused", port, "second", "claude-t").await;
        let routed = anthropic_messages_stream(
            &ctx,
            json!({"model":"anthropic/claude-t","messages":[]}),
            utility_policy(&ctx, "anthropic/claude-t").await.as_ref(),
        )
        .await
        .unwrap();
        assert_eq!(routed.selection.connection_id, "accepted");
        assert_eq!(
            routed.selection.reason,
            crate::llm_router::provenance::RouteSelectionReason::Ordered
        );
        let events = collect_stream(routed.events).await;
        assert_eq!(stream_text(&events), "partial");
        assert!(events.iter().any(|(name, _)| name == "error"));
    }

    #[tokio::test]
    async fn route_selection_continuation_carries_order_and_failure_reason() {
        let port = provenance_test_server(SSE_ERROR_BEFORE_CONTENT, SSE_OK_STREAM).await;
        let ctx = test_ctx().await;
        add_provenance_account(&ctx, "failed", "Failed", port, "first", "claude-t").await;
        add_provenance_account(
            &ctx,
            "continued",
            "Continued",
            port,
            "second",
            "claude-next",
        )
        .await;
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "continuation".into(),
                name: "free".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-t".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-next".into(),
                        effort: None,
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let routed = anthropic_messages_stream(
            &ctx,
            json!({"model":"anthropic/claude-t","messages":[]}),
            utility_policy(&ctx, "anthropic/claude-t").await.as_ref(),
        )
        .await
        .unwrap();
        assert_eq!(routed.selection.resolved_model, "claude-next");
        assert_eq!(
            routed.selection.reason,
            crate::llm_router::provenance::RouteSelectionReason::Failover(
                RouteFailureCategory::Unavailable
            )
        );
    }

    /// Concatenated text of every text_delta in the collected events.
    fn stream_text(events: &[AnthropicEvent]) -> String {
        events
            .iter()
            .filter(|(name, _)| name.as_str() == "content_block_delta")
            .filter_map(|(_, data)| data["delta"]["text"].as_str())
            .collect()
    }

    /// A complete, well-terminated Anthropic SSE stream ("rotated").
    const SSE_OK_STREAM: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_ok\",\"model\":\"claude-t\",\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"rotated\"}}\n\n",
        "event: content_block_stop\n",
        "data: {\"type\":\"content_block_stop\",\"index\":0}\n\n",
        "event: message_delta\n",
        "data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"output_tokens\":2}}\n\n",
        "event: message_stop\n",
        "data: {\"type\":\"message_stop\"}\n\n",
    );

    #[test]
    fn transport_failures_are_always_retryable() {
        let transport = UpstreamAttemptFailure::transport("anthropic", "connection refused");
        assert!(should_try_next_target(&transport));

        // A plain 400 without a retryable message stays non-retryable.
        let bad_request = UpstreamAttemptFailure {
            provider: "anthropic".into(),
            message: "invalid request".into(),
            status: Some(400),
            category: RouteFailureCategory::Unavailable,
        };
        assert!(!should_try_next_target(&bad_request));
    }

    #[tokio::test]
    async fn anthropic_messages_rotates_past_transport_send_error() {
        use axum::{routing::post, Json, Router};

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

        // A port with nothing listening: bind, read the port, drop the listener.
        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead);

        let app = Router::new().route("/messages", post(ok));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "dead",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-dead".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{dead_port}")),
                    models_override: Some(vec!["claude-t".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "live",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-live".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}")),
                    models_override: Some(vec!["claude-t".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let response = anthropic_messages(
            &ctx,
            json!({
                "model": "anthropic/claude-t",
                "messages": [{"role": "user", "content": "hi"}],
            }),
        )
        .await
        .unwrap();

        assert_eq!(response["content"][0]["text"], "fallback worked");
    }

    #[tokio::test]
    async fn stream_rotates_past_transport_send_error() {
        use axum::{routing::post, Router};

        let dead = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_port = dead.local_addr().unwrap().port();
        drop(dead);

        let app = Router::new().route(
            "/messages",
            post(|| async { ([("content-type", "text/event-stream")], SSE_OK_STREAM) }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "dead",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-dead".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{dead_port}")),
                    models_override: Some(vec!["claude-t".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "live",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-live".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}")),
                    models_override: Some(vec!["claude-t".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let rx = anthropic_messages_stream(
            &ctx,
            json!({
                "model": "anthropic/claude-t",
                "messages": [{"role": "user", "content": "hi"}],
            }),
            utility_policy(&ctx, "anthropic/claude-t").await.as_ref(),
        )
        .await
        .unwrap();
        let events = collect_stream(rx.events).await;

        assert_eq!(stream_text(&events), "rotated");
    }

    /// 2xx stream that errors before any content event.
    const SSE_ERROR_BEFORE_CONTENT: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: error\n",
        "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
    );

    /// 2xx stream that errors AFTER content already flowed.
    const SSE_CONTENT_THEN_ERROR: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"partial\"}}\n\n",
        "event: error\n",
        "data: {\"type\":\"error\",\"error\":{\"type\":\"overloaded_error\",\"message\":\"Overloaded\"}}\n\n",
    );

    /// Two anthropic api_key accounts serving `claude-t`, first→/first,
    /// second→/second on the given mock port.
    async fn add_two_anthropic_accounts(ctx: &UpstreamCtx, port: u16) {
        for (id, path) in [("first", "first"), ("second", "second")] {
            connections::add_connection(
                &ctx.store,
                mk_conn(
                    id,
                    "anthropic",
                    "api_key",
                    ConnectionData {
                        api_key: Some(format!("sk-{id}")),
                        base_url_override: Some(format!("http://127.0.0.1:{port}/{path}")),
                        models_override: Some(vec!["claude-t".into()]),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
    }

    #[tokio::test]
    async fn repeated_streams_route_accounts_dynamically_with_same_effort_policy() {
        use axum::{routing::post, Router};

        const FIRST: &str = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"first\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        const SECOND: &str = "event: content_block_delta\ndata: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"second\"}}\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n";
        let app = Router::new()
            .route(
                "/first/messages",
                post(|| async { ([("content-type", "text/event-stream")], FIRST) }),
            )
            .route(
                "/second/messages",
                post(|| async { ([("content-type", "text/event-stream")], SECOND) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });

        let ctx = test_ctx().await;
        add_two_anthropic_accounts(&ctx, port).await;
        routes::save_provider_account_route(
            &ctx.store,
            "anthropic",
            routes::ModelRouteStrategy::RoundRobin,
        )
        .await
        .unwrap();
        let policy = utility_policy(&ctx, "anthropic/claude-t").await;
        let body = || json!({"model": "anthropic/claude-t", "messages": [{"role": "user", "content": "hi"}]});

        let first = anthropic_messages_stream(&ctx, body(), &policy)
            .await
            .unwrap();
        let second = anthropic_messages_stream(&ctx, body(), &policy)
            .await
            .unwrap();
        assert_eq!(stream_text(&collect_stream(first.events).await), "first");
        assert_eq!(stream_text(&collect_stream(second.events).await), "second");
        assert_eq!(Arc::strong_count(&policy), 1);
    }

    #[tokio::test]
    async fn stream_rotates_on_error_event_before_first_content() {
        use axum::{routing::post, Router};

        let app = Router::new()
            .route(
                "/first/messages",
                post(|| async {
                    (
                        [("content-type", "text/event-stream")],
                        SSE_ERROR_BEFORE_CONTENT,
                    )
                }),
            )
            .route(
                "/second/messages",
                post(|| async { ([("content-type", "text/event-stream")], SSE_OK_STREAM) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        add_two_anthropic_accounts(&ctx, port).await;

        let rx = anthropic_messages_stream(
            &ctx,
            json!({"model": "anthropic/claude-t", "messages": [{"role": "user", "content": "hi"}]}),
            utility_policy(&ctx, "anthropic/claude-t").await.as_ref(),
        )
        .await
        .unwrap();
        let events = collect_stream(rx.events).await;

        assert_eq!(stream_text(&events), "rotated");
        assert!(
            !events.iter().any(|(name, _)| name.as_str() == "error"),
            "the first target's pre-content error must not leak to the consumer: {events:?}"
        );
    }

    #[tokio::test]
    async fn stream_does_not_rotate_after_first_content_token() {
        use axum::{routing::post, Router};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let second_hits = Arc::new(AtomicUsize::new(0));
        let second_hits_handler = second_hits.clone();
        let app = Router::new()
            .route(
                "/first/messages",
                post(|| async {
                    (
                        [("content-type", "text/event-stream")],
                        SSE_CONTENT_THEN_ERROR,
                    )
                }),
            )
            .route(
                "/second/messages",
                post(move || {
                    let hits = second_hits_handler.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        ([("content-type", "text/event-stream")], SSE_OK_STREAM)
                    }
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        add_two_anthropic_accounts(&ctx, port).await;

        let rx = anthropic_messages_stream(
            &ctx,
            json!({"model": "anthropic/claude-t", "messages": [{"role": "user", "content": "hi"}]}),
            utility_policy(&ctx, "anthropic/claude-t").await.as_ref(),
        )
        .await
        .unwrap();
        let events = collect_stream(rx.events).await;

        assert_eq!(stream_text(&events), "partial");
        assert!(
            events
                .iter()
                .any(|(name, data)| name.as_str() == "error"
                    && data["error"]["message"] == "Overloaded"),
            "post-content errors surface to the consumer instead of rotating: {events:?}"
        );
        assert_eq!(
            second_hits.load(Ordering::SeqCst),
            0,
            "must not fail over once content has been delivered"
        );
    }

    /// When every target's stream errors before any content, the call
    /// returns `Err` (the aggregated fallback error) instead of handing back
    /// a stream that would carry the last target's error event verbatim.
    #[tokio::test]
    async fn stream_returns_err_when_every_target_fails_pre_content() {
        use axum::{routing::post, Router};

        let app = Router::new()
            .route(
                "/first/messages",
                post(|| async {
                    (
                        [("content-type", "text/event-stream")],
                        SSE_ERROR_BEFORE_CONTENT,
                    )
                }),
            )
            .route(
                "/second/messages",
                post(|| async {
                    (
                        [("content-type", "text/event-stream")],
                        SSE_ERROR_BEFORE_CONTENT,
                    )
                }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        add_two_anthropic_accounts(&ctx, port).await;

        let err = anthropic_messages_stream(
            &ctx,
            json!({"model": "anthropic/claude-t", "messages": [{"role": "user", "content": "hi"}]}),
            utility_policy(&ctx, "anthropic/claude-t").await.as_ref(),
        )
        .await
        .err()
        .expect("all attempts must fail");

        assert!(
            err.to_string().contains("Overloaded"),
            "expected the aggregated fallback error to mention both attempts' failures: {err}"
        );
    }

    /// 2xx stream with content but NO terminal event (no message_stop).
    const SSE_TRUNCATED_NO_TERMINAL: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":1}}}\n\n",
        "event: content_block_start\n",
        "data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"text\",\"text\":\"\"}}\n\n",
        "event: content_block_delta\n",
        "data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"half\"}}\n\n",
    );

    /// 2xx stream that ends after message_start — no content, no terminal.
    const SSE_MESSAGE_START_ONLY: &str = concat!(
        "event: message_start\n",
        "data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"usage\":{\"input_tokens\":1}}}\n\n",
    );

    #[tokio::test]
    async fn stream_surfaces_error_when_2xx_stream_ends_without_terminal_event() {
        use axum::{routing::post, Router};

        let app = Router::new().route(
            "/messages",
            post(|| async {
                (
                    [("content-type", "text/event-stream")],
                    SSE_TRUNCATED_NO_TERMINAL,
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "only",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-only".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}")),
                    models_override: Some(vec!["claude-t".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let rx = anthropic_messages_stream(
            &ctx,
            json!({"model": "anthropic/claude-t", "messages": [{"role": "user", "content": "hi"}]}),
            utility_policy(&ctx, "anthropic/claude-t").await.as_ref(),
        )
        .await
        .unwrap();
        let events = collect_stream(rx.events).await;

        // The content that DID arrive is preserved…
        assert_eq!(stream_text(&events), "half");
        // …and the silent truncation becomes an explicit terminal error.
        let (last_name, last_data) = events.last().expect("stream must not be empty");
        assert_eq!(last_name, "error");
        assert_eq!(
            last_data["error"]["message"],
            "upstream stream ended without a terminal event"
        );
    }

    #[tokio::test]
    async fn stream_rotates_past_stream_missing_terminal_before_content() {
        use axum::{routing::post, Router};

        let app = Router::new()
            .route(
                "/first/messages",
                post(|| async {
                    (
                        [("content-type", "text/event-stream")],
                        SSE_MESSAGE_START_ONLY,
                    )
                }),
            )
            .route(
                "/second/messages",
                post(|| async { ([("content-type", "text/event-stream")], SSE_OK_STREAM) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        add_two_anthropic_accounts(&ctx, port).await;

        let rx = anthropic_messages_stream(
            &ctx,
            json!({"model": "anthropic/claude-t", "messages": [{"role": "user", "content": "hi"}]}),
            utility_policy(&ctx, "anthropic/claude-t").await.as_ref(),
        )
        .await
        .unwrap();
        let events = collect_stream(rx.events).await;

        // The empty first stream (no terminal event, no content) triggers the
        // guard's error event, which the pre-content probe converts into a
        // rotation to the second account.
        assert_eq!(stream_text(&events), "rotated");
        assert!(
            !events.iter().any(|(name, _)| name.as_str() == "error"),
            "empty truncated stream must rotate, not surface: {events:?}"
        );
    }

    #[test]
    fn strip_thinking_removes_the_key_for_anthropic_native_upstreams() {
        // Shape the runner sends when `thinking_budget` fires (spec §8):
        // `thinking: {type: "enabled", budget_tokens: N}` alongside the rest
        // of the Anthropic Messages body.
        let mut body = json!({
            "model": "claude-sonnet-5",
            "messages": [],
            "max_tokens": 4096,
            "thinking": {"type": "enabled", "budget_tokens": 8192},
        });
        strip_thinking(&mut body);
        assert!(
            body.get("thinking").is_none(),
            "thinking key must be removed before an Anthropic-native send"
        );
        // Other keys are untouched.
        assert_eq!(body["model"], "claude-sonnet-5");
        assert_eq!(body["max_tokens"], 4096);
    }

    #[test]
    fn strip_thinking_is_a_no_op_when_absent() {
        let mut body = json!({"model": "m", "messages": []});
        strip_thinking(&mut body);
        assert_eq!(body["model"], "m");
    }

    #[test]
    fn apply_max_completion_tokens_renames_only_for_openai() {
        let mut body = json!({"model": "gpt-5.2", "max_tokens": 64, "messages": []});
        apply_max_completion_tokens(registry::descriptor("openai").unwrap(), &mut body);
        assert_eq!(body["max_completion_tokens"], 64);
        assert!(body.get("max_tokens").is_none());

        for id in ["mimo-free", "qwen", "github-copilot", "deepseek"] {
            let mut body = json!({"model": "m", "max_tokens": 64, "messages": []});
            apply_max_completion_tokens(registry::descriptor(id).unwrap(), &mut body);
            assert_eq!(body["max_tokens"], 64, "{id} must keep max_tokens");
            assert!(
                body.get("max_completion_tokens").is_none(),
                "{id} must not gain max_completion_tokens"
            );
        }
    }

    #[tokio::test]
    async fn anthropic_messages_renames_max_tokens_for_openai_descriptor() {
        use axum::{routing::post, Json, Router};

        async fn ok(Json(body): Json<Value>) -> Json<Value> {
            assert_eq!(body["max_completion_tokens"], 32);
            assert!(body.get("max_tokens").is_none());
            Json(json!({
                "id": "chatcmpl-1", "object": "chat.completion", "model": body["model"].clone(),
                "choices": [{"index": 0, "finish_reason": "stop",
                             "message": {"role": "assistant", "content": "pong"}}],
                "usage": {"prompt_tokens": 1, "completion_tokens": 1}
            }))
        }

        let app = Router::new().route("/v1/chat/completions", post(ok));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "oai",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-oai".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}/v1")),
                    models_override: Some(vec!["gpt-5.2".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let response = anthropic_messages(
            &ctx,
            json!({
                "model": "openai/gpt-5.2",
                "max_tokens": 32,
                "messages": [{"role": "user", "content": "hi"}],
            }),
        )
        .await
        .unwrap();

        assert_eq!(response["content"][0]["text"], "pong");
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
            route_target_key: None,
        };
        let body = json!({
            "model": "claude-x",
            "system": "be helpful",
            "messages": [],
            "tools": [{
                "name": "task",
                "input_schema": {
                    "type": "object",
                    "oneOf": [
                        {
                            "properties": {"prompt": {"type": "string"}},
                            "required": ["prompt"]
                        },
                        {
                            "properties": {"tasks": {"type": "array"}},
                            "required": ["tasks"]
                        }
                    ]
                }
            }]
        });
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
        assert!(sent["system"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("x-anthropic-billing-header: cc_version=2.1.92."));
        assert_eq!(sent["system"][1]["text"], CLAUDE_CODE_SYSTEM_PROMPT);
        assert_eq!(sent["system"][2]["text"], "be helpful");
        let schema = &sent["tools"][0]["input_schema"];
        assert!(schema.get("oneOf").is_none());
        assert!(schema["properties"].get("prompt").is_some());
        assert!(schema["properties"].get("tasks").is_some());
    }

    #[tokio::test]
    async fn anthropic_oauth_without_config_cloaks_request_and_headers() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("anthropic-oauth").unwrap();
        let conn = mk_conn(
            "c1",
            "anthropic-oauth",
            "oauth",
            ConnectionData {
                access_token: Some("sk-ant-oat-test".into()),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "claude-x".into(),
            route_target_key: None,
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
            route_target_key: None,
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
            route_target_key: None,
        };
        let body = json!({"model": "gpt-5.2-codex", "input": "hi"});
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert!(req.headers().get("chatgpt-account-id").is_none());
    }

    #[tokio::test]
    async fn anthropic_messages_route_serves_codex_route_target_now_that_its_drivable() {
        // Codex is now natively drivable (via `codex_stream`), so a route
        // listing it first is no longer skipped at the routing layer — it's
        // served, same as any other provider.
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
                        provider: "openai".into(),
                        model: "gpt-5.2-codex".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-sonnet-4-5".into(),
                        effort: None,
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

        assert_eq!(target.conn.id, "chatgpt");
        assert_eq!(target.conn.provider, "openai-oauth");
        assert_eq!(target.upstream_model, "gpt-5.2-codex");
    }

    #[tokio::test]
    async fn anthropic_messages_route_skips_uncredentialed_route_target() {
        // An openai-oauth connection with no token still can't actually be
        // driven, so it's skipped in favor of the next fallback target —
        // same as any other provider missing credentials.
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "chatgpt",
                "openai-oauth",
                "oauth",
                ConnectionData {
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
                        provider: "openai".into(),
                        model: "gpt-5.2-codex".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-sonnet-4-5".into(),
                        effort: None,
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
    async fn selectable_models_expose_pinned_anthropic_fallback_without_route_edits() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        connections::add_connection(
            &store,
            ConnectionRow {
                id: "anthropic-selectable".into(),
                provider: "anthropic".into(),
                auth_type: "api_key".into(),
                label: "Anthropic".into(),
                priority: 0,
                enabled: true,
                data: ConnectionData {
                    api_key: Some("test-key".into()),
                    models_override: Some(vec!["claude-opus-4-5".into(), "claude-opus-4-7".into()]),
                    ..Default::default()
                },
                created_at: 0,
                updated_at: 0,
            },
        )
        .await
        .unwrap();

        let models = selectable_native_models(&store).await.unwrap();
        let opus_45 = models
            .iter()
            .find(|model| model.request_value == "anthropic/claude-opus-4-5")
            .unwrap();
        let opus_47 = models
            .iter()
            .find(|model| model.request_value == "anthropic/claude-opus-4-7")
            .unwrap();
        assert_eq!(
            opus_45
                .supported
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "medium", "high"],
        );
        assert_eq!(
            opus_47
                .supported
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["low", "medium", "high", "max", "xhigh"],
        );
        assert_eq!(opus_47.resolved_default.as_deref(), Some("high"));
    }

    #[tokio::test]
    async fn selectable_native_models_returns_one_canonical_codex_model_with_metadata() {
        let ctx = test_ctx().await;
        let mut model_meta = std::collections::HashMap::new();
        model_meta.insert(
            "gpt-5.2-codex".into(),
            crate::llm_router::model_effort::DiscoveredModelMeta {
                display_name: Some("GPT-5.2 Codex".into()),
                effort_options: Some(vec![
                    crate::llm_router::model_effort::ReasoningEffortOption {
                        value: "medium".into(),
                        label: "Medium".into(),
                        description: Some("Balanced".into()),
                    },
                    crate::llm_router::model_effort::ReasoningEffortOption {
                        value: "high".into(),
                        label: "High".into(),
                        description: Some("More reasoning".into()),
                    },
                ]),
                default_effort_advertised: true,
                default_effort: Some("medium".into()),
            },
        );
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "cx",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("at".into()),
                    models_override: Some(vec!["gpt-5.2-codex".into()]),
                    model_meta_overrides: Some(model_meta),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let models = selectable_native_models(&ctx.store).await.unwrap();
        assert_eq!(models.len(), 1, "must not synthesize effort model ids");
        let model = &models[0];
        assert_eq!(model.request_value, "openai/gpt-5.2-codex");
        assert_eq!(model.display_name, "GPT-5.2 Codex");
        assert_eq!(
            model
                .supported
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["medium", "high"]
        );
        assert_eq!(model.resolved_default.as_deref(), Some("medium"));
        assert!(!models.iter().any(|model| {
            ["-low", "-medium", "-high", "-xhigh"]
                .iter()
                .any(|suffix| model.request_value.ends_with(suffix))
        }));
    }

    #[tokio::test]
    async fn legacy_codex_family_requests_are_canonicalized_per_oauth_candidate() {
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "generic",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("sk".into()),
                    models_override: Some(vec!["gpt-5.5-review".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "codex",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("at".into()),
                    models_override: Some(vec!["gpt-5.5".into(), "gpt-5.5-review".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        for request in ["openai/gpt-5.5-high-review", "openai/gpt-5.5-review-high"] {
            let targets = route_models_for_anthropic_messages(&ctx.store, request)
                .await
                .unwrap();
            assert_eq!(
                targets.len(),
                1,
                "generic OpenAI must not accept compatibility parsing"
            );
            assert_eq!(targets[0].conn.provider, "openai-oauth");
            assert_eq!(targets[0].upstream_model, "gpt-5.5-review");
            assert!(targets[0].route_target_key.is_none());
        }
    }

    #[tokio::test]
    async fn named_route_targets_keep_original_effort_key_after_round_robin_rotation() {
        let ctx = test_ctx().await;
        for (id, provider, model) in [
            ("openai", "openai", "gpt-route"),
            ("anthropic", "anthropic", "claude-route"),
        ] {
            connections::add_connection(
                &ctx.store,
                mk_conn(
                    id,
                    provider,
                    "api_key",
                    ConnectionData {
                        api_key: Some("key".into()),
                        models_override: Some(vec![model.into()]),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r1".into(),
                name: "rotating".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::RoundRobin,
                targets: vec![
                    routes::ModelRouteTarget {
                        provider: "openai".into(),
                        model: "gpt-route".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-route".into(),
                        effort: None,
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let first = route_models_for_anthropic_messages(&ctx.store, "rotating")
            .await
            .unwrap();
        let second = route_models_for_anthropic_messages(&ctx.store, "rotating")
            .await
            .unwrap();
        assert_eq!(first[0].route_target_key.as_ref().unwrap().target_index, 0);
        assert_eq!(second[0].route_target_key.as_ref().unwrap().target_index, 1);
    }

    #[tokio::test]
    async fn matching_route_alias_ending_in_high_remains_exact() {
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "generic",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("sk".into()),
                    models_override: Some(vec!["real-model".into()]),
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
                name: "fast-high".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![routes::ModelRouteTarget {
                    provider: "openai".into(),
                    model: "real-model".into(),
                    effort: None,
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let target = route_model(&ctx.store, "fast-high").await.unwrap().unwrap();
        assert_eq!(target.upstream_model, "real-model");
        assert!(target.route_target_key.is_some());
    }

    #[tokio::test]
    async fn selectable_models_do_not_advance_round_robin_cursors() {
        let ctx = test_ctx().await;
        for id in ["openai-1", "openai-2"] {
            connections::add_connection(
                &ctx.store,
                mk_conn(
                    id,
                    "openai",
                    "api_key",
                    ConnectionData {
                        api_key: Some("key".into()),
                        models_override: Some(vec!["gpt-route".into()]),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "anthropic-1",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("key".into()),
                    models_override: Some(vec!["claude-route".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        routes::save_provider_account_route(
            &ctx.store,
            "openai",
            routes::ModelRouteStrategy::RoundRobin,
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r1".into(),
                name: "rotating".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::RoundRobin,
                targets: vec![
                    routes::ModelRouteTarget {
                        provider: "openai".into(),
                        model: "gpt-route".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-route".into(),
                        effort: None,
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        selectable_native_models(&ctx.store).await.unwrap();
        selectable_native_models(&ctx.store).await.unwrap();

        let direct = route_models_for_anthropic_messages(&ctx.store, "openai/gpt-route")
            .await
            .unwrap();
        assert_eq!(direct[0].conn.id, "openai-1");
        let named = route_models_for_anthropic_messages(&ctx.store, "rotating")
            .await
            .unwrap();
        assert_eq!(named[0].route_target_key.as_ref().unwrap().target_index, 0);
    }

    #[tokio::test]
    async fn route_tool_capabilities_are_adapter_facts_not_model_name_inference() {
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "openai",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("key".into()),
                    models_override: Some(vec!["opaque-alpha".into(), "opaque-beta".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let alpha = route_tool_capabilities(&ctx.store, "openai/opaque-alpha")
            .await
            .unwrap();
        let beta = route_tool_capabilities(&ctx.store, "openai/opaque-beta")
            .await
            .unwrap();

        assert_eq!(alpha, beta);
        assert_eq!(
            alpha.wire_protocol,
            crate::harness::native::capabilities::WireProtocol::OpenAiChat
        );
        assert!(alpha.supports_strict_function_schema);
    }

    #[tokio::test]
    async fn route_tool_capabilities_report_a_typed_error_for_an_empty_target_set() {
        let ctx = test_ctx().await;

        let error = route_tool_capabilities(&ctx.store, "opaque-missing")
            .await
            .unwrap_err();

        assert!(error.downcast_ref::<CapabilityResolutionError>().is_some());
    }

    #[tokio::test]
    async fn openai_base_url_override_is_a_conservative_compatible_endpoint() {
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "override",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("key".into()),
                    base_url_override: Some("https://compatible.example/v1".into()),
                    models_override: Some(vec!["opaque-override".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let capabilities = route_tool_capabilities(&ctx.store, "openai/opaque-override")
            .await
            .unwrap();

        assert!(capabilities.supports_function_tools);
        assert!(!capabilities.supports_custom_freeform_tools);
        assert!(!capabilities.supports_strict_function_schema);
        assert!(!capabilities.supports_tool_output_schema);
    }

    #[tokio::test]
    async fn strict_tools_reject_an_openai_base_url_override() {
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "override",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("key".into()),
                    base_url_override: Some("https://compatible.example/v1".into()),
                    models_override: Some(vec!["opaque-override".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        let frozen_request = json!({
            "tools": [{"type": "function", "function": {
                "name": "lookup",
                "strict": true,
                "parameters": {"type": "object"}
            }}]
        });

        let targets =
            route_models_for_body(&ctx.store, "openai/opaque-override", Some(&frozen_request))
                .await
                .unwrap();

        assert!(targets.is_empty());
    }

    #[tokio::test]
    async fn pinned_route_without_an_initial_target_does_not_peek_at_continuation() {
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "fallback",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("key".into()),
                    models_override: Some(vec!["reachable-fallback".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "unreachable-pinned-start".into(),
                name: "unreachable-pinned-start".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![
                    routes::ModelRouteTarget {
                        provider: "openai".into(),
                        model: "missing-initial".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "reachable-fallback".into(),
                        effort: None,
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let error = route_tool_capabilities(&ctx.store, "openai/missing-initial")
            .await
            .unwrap_err();

        assert!(error.downcast_ref::<CapabilityResolutionError>().is_some());
    }

    #[tokio::test]
    async fn route_tool_capabilities_intersect_fallbacks_without_advancing_order() {
        let ctx = test_ctx().await;
        let custom_id =
            crate::llm_router::custom::add_custom_provider(&ctx.store, "Capability Fallback")
                .await
                .unwrap()[0]
                .id
                .clone();
        for id in ["openai-1", "openai-2"] {
            connections::add_connection(
                &ctx.store,
                mk_conn(
                    id,
                    "openai",
                    "api_key",
                    ConnectionData {
                        api_key: Some("key".into()),
                        models_override: Some(vec!["opaque-primary".into()]),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "custom",
                &custom_id,
                "api_key",
                ConnectionData {
                    api_key: Some("key".into()),
                    base_url_override: Some("http://127.0.0.1:9/v1".into()),
                    models_override: Some(vec!["opaque-fallback".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        routes::save_provider_account_route(
            &ctx.store,
            "openai",
            routes::ModelRouteStrategy::RoundRobin,
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "mixed-route".into(),
                name: "mixed".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::RoundRobin,
                targets: vec![
                    routes::ModelRouteTarget {
                        provider: "openai".into(),
                        model: "opaque-primary".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: custom_id,
                        model: "opaque-fallback".into(),
                        effort: None,
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let capabilities = route_tool_capabilities(&ctx.store, "mixed").await.unwrap();
        assert!(!capabilities.supports_strict_function_schema);
        assert_eq!(
            capabilities.wire_protocol,
            crate::harness::native::capabilities::WireProtocol::OpenAiChat
        );

        let routed = route_models_for_anthropic_messages(&ctx.store, "mixed")
            .await
            .unwrap();
        assert_eq!(routed[0].conn.id, "openai-1");
        assert_eq!(routed[0].route_target_key.as_ref().unwrap().target_index, 0);
    }

    #[tokio::test]
    async fn dynamic_free_route_intersects_and_filters_declared_transport_facts() {
        let ctx = test_ctx().await;
        for (id, provider, model) in [
            ("mimo-free-account", "mimo-free", "mimo-auto"),
            ("opencode-free-account", "opencode-free", "grok-code"),
        ] {
            connections::add_connection(
                &ctx.store,
                mk_conn(
                    id,
                    provider,
                    "free",
                    ConnectionData {
                        models_override: Some(vec![model.into()]),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "dynamic-free-route".into(),
                name: "free".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![
                    routes::ModelRouteTarget {
                        provider: "mimo-free".into(),
                        model: "mimo-auto".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "opencode-free".into(),
                        model: "grok-code".into(),
                        effort: None,
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let capabilities = route_tool_capabilities(&ctx.store, "free").await.unwrap();
        assert_eq!(
            capabilities.wire_protocol,
            crate::harness::native::capabilities::WireProtocol::OpenAiChat
        );
        assert!(capabilities.supports_function_tools);
        assert!(!capabilities.supports_strict_function_schema);

        let strict_request = json!({"tools": [{"type": "function", "function": {
            "name": "lookup",
            "strict": true,
            "parameters": {"type": "object"}
        }}]});
        assert!(
            route_models_for_body(&ctx.store, "free", Some(&strict_request))
                .await
                .unwrap()
                .is_empty()
        );
    }

    #[tokio::test]
    async fn strict_tool_requirements_hard_filter_incompatible_route_targets() {
        let ctx = test_ctx().await;
        let custom_id =
            crate::llm_router::custom::add_custom_provider(&ctx.store, "Strict Incompatible")
                .await
                .unwrap()[0]
                .id
                .clone();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "custom",
                &custom_id,
                "api_key",
                ConnectionData {
                    api_key: Some("key".into()),
                    base_url_override: Some("http://127.0.0.1:9/v1".into()),
                    models_override: Some(vec!["opaque-custom".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "official",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("key".into()),
                    models_override: Some(vec!["opaque-official".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "strict-route".into(),
                name: "strict".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![
                    routes::ModelRouteTarget {
                        provider: custom_id,
                        model: "opaque-custom".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "openai".into(),
                        model: "opaque-official".into(),
                        effort: None,
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let frozen_request = json!({
            "model": "strict",
            "messages": [],
            "tools": [{
                "type": "function",
                "function": {
                    "name": "lookup",
                    "strict": true,
                    "parameters": {"type": "object"}
                }
            }]
        });

        let targets = route_models_for_body(&ctx.store, "strict", Some(&frozen_request))
            .await
            .unwrap();

        assert_eq!(targets.len(), 1);
        assert_eq!(targets[0].conn.id, "official");
    }

    #[tokio::test]
    async fn custom_and_output_requirements_keep_only_openai_responses_retry_targets() {
        let ctx = test_ctx().await;
        for (id, provider, auth_type, access_token) in [
            ("chat", "openai", "api_key", None),
            ("responses", "openai-oauth", "oauth", Some("token")),
        ] {
            connections::add_connection(
                &ctx.store,
                mk_conn(
                    id,
                    provider,
                    auth_type,
                    ConnectionData {
                        api_key: (provider == "openai").then(|| "key".into()),
                        access_token: access_token.map(str::to_string),
                        models_override: Some(vec!["opaque-tools".into()]),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "responses-tools".into(),
                name: "responses-tools".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![routes::ModelRouteTarget {
                    provider: "openai".into(),
                    model: "opaque-tools".into(),
                    effort: None,
                }],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let custom = json!({"tools": [{"type": "custom", "custom": {"name": "shell"}}]});
        let output = json!({"tools": [{"type": "function", "function": {
            "name": "lookup",
            "output_schema": {"type": "string"}
        }}]});
        for request in [&custom, &output] {
            let targets = route_models_for_body(&ctx.store, "responses-tools", Some(request))
                .await
                .unwrap();
            assert_eq!(targets.len(), 1);
            assert_eq!(targets[0].conn.id, "responses");
        }
    }

    #[tokio::test]
    async fn continuation_queue_rejects_strict_custom_and_output_for_an_override_endpoint() {
        let ctx = test_ctx().await;
        for (id, provider, model, base_url_override) in [
            ("initial", "anthropic", "start", None),
            (
                "fallback",
                "openai",
                "fallback",
                Some("https://compatible.example/v1"),
            ),
        ] {
            connections::add_connection(
                &ctx.store,
                mk_conn(
                    id,
                    provider,
                    "api_key",
                    ConnectionData {
                        api_key: Some("key".into()),
                        base_url_override: base_url_override.map(str::to_string),
                        models_override: Some(vec![model.into()]),
                        ..Default::default()
                    },
                ),
            )
            .await
            .unwrap();
        }
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "continuation-requirements".into(),
                name: "continuation-requirements".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "start".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "openai".into(),
                        model: "fallback".into(),
                        effort: None,
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();
        let attempted = std::collections::HashSet::from([("initial".into(), "start".into())]);

        for requirements in [
            capabilities::ToolTransportRequirements {
                strict_function_schema: true,
                ..Default::default()
            },
            capabilities::ToolTransportRequirements {
                custom_freeform_tools: true,
                ..Default::default()
            },
            capabilities::ToolTransportRequirements {
                tool_output_schema: true,
                ..Default::default()
            },
        ] {
            let mut order_cache = ProviderOrderCache::new();
            let targets = route_continuation_targets(
                &ctx.store,
                "anthropic/start",
                &attempted,
                &mut order_cache,
                requirements,
                RouteOrderMode::Advance,
            )
            .await
            .unwrap();
            assert!(targets.is_empty(), "requirements: {requirements:?}");
        }
    }

    #[tokio::test]
    async fn compatibility_suffixes_are_candidate_local_and_unknown_models_remain_exact() {
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "codex",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("at".into()),
                    models_override: Some(vec!["gpt-known".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "generic-bare",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("sk".into()),
                    models_override: Some(vec!["gpt-known-high".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "generic-openai",
                "openai",
                "api_key",
                ConnectionData {
                    api_key: Some("sk".into()),
                    models_override: Some(vec!["gpt-unknown-high".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "anthropic",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk".into()),
                    models_override: Some(vec!["claude-high".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let bare = route_models_for_anthropic_messages(&ctx.store, "gpt-known-high")
            .await
            .unwrap();
        assert_eq!(bare.len(), 1);
        assert_eq!(bare[0].conn.id, "generic-bare");
        assert_eq!(bare[0].upstream_model, "gpt-known-high");

        let unknown = route_model(&ctx.store, "openai/gpt-unknown-high")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(unknown.conn.provider, "openai");
        assert_eq!(unknown.upstream_model, "gpt-unknown-high");

        let anthropic = route_model(&ctx.store, "anthropic/claude-high")
            .await
            .unwrap()
            .unwrap();
        assert_eq!(anthropic.upstream_model, "claude-high");
    }

    #[tokio::test]
    async fn selectable_native_models_intersects_routes_and_preserves_review_identity_and_defaults()
    {
        let ctx = test_ctx().await;
        let discovered =
            |values: &[&str], default: &str| crate::llm_router::model_effort::DiscoveredModelMeta {
                display_name: Some("Friendly base".into()),
                effort_options: Some(
                    values
                        .iter()
                        .map(|value| model_effort::ReasoningEffortOption {
                            value: (*value).into(),
                            label: (*value).into(),
                            description: None,
                        })
                        .collect(),
                ),
                default_effort_advertised: true,
                default_effort: Some(default.into()),
            };
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "codex",
                "openai-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("at".into()),
                    models_override: Some(vec!["gpt-x".into(), "gpt-x-review".into()]),
                    model_meta_overrides: Some(
                        [
                            ("gpt-x".into(), discovered(&["medium", "high"], "medium")),
                            (
                                "gpt-x-review".into(),
                                discovered(&["medium", "high"], "medium"),
                            ),
                        ]
                        .into(),
                    ),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "anthropic",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk".into()),
                    models_override: Some(vec!["claude-x".into()]),
                    model_meta_overrides: Some(
                        [("claude-x".into(), discovered(&["high", "ultra"], "high"))].into(),
                    ),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        let route_info = routes::ModelRouteInfo {
            id: "r1".into(),
            name: "safe".into(),
            enabled: true,
            strategy: routes::ModelRouteStrategy::Fallback,
            targets: vec![
                routes::ModelRouteTarget {
                    provider: "openai".into(),
                    model: "gpt-x".into(),
                    effort: Some("high".into()),
                },
                routes::ModelRouteTarget {
                    provider: "anthropic".into(),
                    model: "claude-x".into(),
                    effort: None,
                },
            ],
            created_at: 1,
            updated_at: 1,
        };
        ctx.store
            .set_setting(
                crate::domain::WriteOrigin::User,
                "llm_model_routes",
                &serde_json::to_string(&vec![route_info]).unwrap(),
            )
            .await
            .unwrap();
        let models = selectable_native_models(&ctx.store).await.unwrap();
        let route = models
            .iter()
            .find(|model| model.request_value == "safe")
            .unwrap();
        assert_eq!(
            route
                .supported
                .iter()
                .map(|option| option.value.as_str())
                .collect::<Vec<_>>(),
            vec!["high"]
        );
        assert_eq!(route.configured_default, None);
        assert_eq!(route.resolved_default.as_deref(), Some("high"));
        assert_eq!(
            route.default_source,
            model_effort::ModelDefaultSource::VariesByTarget
        );
        let policy = model_effort::build_utility_effort_policy(&ctx.store, "safe")
            .await
            .unwrap();
        let routed = route_models_for_anthropic_messages(&ctx.store, "safe")
            .await
            .unwrap();
        let target = routed
            .iter()
            .find(|target| target.conn.provider == "openai-oauth")
            .unwrap();
        let surface = model_effort::ExecutionSurfaceKey {
            provider_id: target.conn.provider.clone(),
            connection_id: Some(target.conn.id.clone()),
            model: target.upstream_model.clone(),
        };
        let preference = model_effort::ModelPreferenceKey {
            family: target.desc.family.into(),
            model: target.upstream_model.clone(),
        };
        let resolved = resolve_target_effort(
            &policy,
            target.route_target_key.as_ref(),
            &preference,
            &surface,
        );
        assert_eq!(resolved.value.as_deref(), Some("high"));
        assert_eq!(
            resolved.source,
            model_effort::EffectiveEffortSource::RouteTarget
        );
        let mut project_policy = policy.clone();
        project_policy.caller_override = Some("medium".into());
        let project = resolve_target_effort(
            &project_policy,
            target.route_target_key.as_ref(),
            &preference,
            &surface,
        );
        assert_eq!(project.value.as_deref(), Some("high"));
        assert_eq!(
            project.source,
            model_effort::EffectiveEffortSource::RouteTarget
        );
        let concrete = models
            .iter()
            .find(|model| model.request_value == "openai/gpt-x")
            .unwrap();
        assert_eq!(concrete.configured_default, None);
        assert_eq!(concrete.resolved_default.as_deref(), Some("medium"));
        assert_eq!(
            concrete.default_source,
            model_effort::ModelDefaultSource::Provider
        );
        let review = models
            .iter()
            .find(|model| model.request_value == "openai/gpt-x-review")
            .unwrap();
        assert_eq!(review.display_name, "gpt-x-review");
    }

    #[tokio::test]
    async fn selectable_native_models_lists_usable_routes_then_connection_models_and_skips_unreachable(
    ) {
        let ctx = test_ctx().await;
        // Codex (openai-oauth) IS drivable natively now (via `codex_stream`),
        // so its models and any route pointing at it must be offered.
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
        // — both are drivable, so both must be offered.
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r1".into(),
                name: "usable-combo".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![routes::ModelRouteTarget {
                    provider: "openrouter".into(),
                    model: "deepseek/deepseek-chat:free".into(),
                    effort: None,
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
                    provider: "openai".into(),
                    model: "gpt-5.2-codex".into(),
                    effort: None,
                }],
                created_at: 2,
                updated_at: 2,
            },
        )
        .await
        .unwrap();

        let models = selectable_native_models(&ctx.store).await.unwrap();

        // Usable route first; the openrouter provider/model is offered.
        assert_eq!(
            models.first().map(|model| model.request_value.as_str()),
            Some("usable-combo")
        );
        assert!(models
            .iter()
            .any(|m| m.request_value == "openrouter/deepseek/deepseek-chat:free"));
        // Kiro is drivable natively — its models are offered.
        assert!(
            models
                .iter()
                .any(|m| m.request_value == "kiro/claude-sonnet-5"),
            "kiro must be offered, got: {models:?}"
        );
        // Codex is drivable natively — its model (plus effort variants) and
        // the Codex-only route are offered.
        assert!(
            models
                .iter()
                .any(|m| m.request_value == "openai/gpt-5.2-codex"),
            "got: {models:?}"
        );
        assert!(
            !models
                .iter()
                .any(|m| m.request_value == "openai/gpt-5.2-codex-high"),
            "selection must not synthesize effort variants: {models:?}"
        );
        assert!(
            models.iter().any(|m| m.request_value == "codex-only"),
            "codex-only route must be offered, got: {models:?}"
        );
        // A keyless connection's models are never offered.
        assert!(
            !models
                .iter()
                .any(|m| m.request_value == "openrouter/keyless/model"),
            "got: {models:?}"
        );
    }

    #[tokio::test]
    async fn model_route_expands_to_every_family_account_in_priority_order() {
        // A route target names a family, not a connection — every enabled
        // account in that family serving the model is a fallback candidate,
        // in connection-priority order (creation order here), regardless of
        // which specific account the route was originally created against.
        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "first-account",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-first".into()),
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
                "second-account",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-second".into()),
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
                    provider: "anthropic".into(),
                    model: "claude-fable-5".into(),
                    effort: None,
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

        assert_eq!(ids, vec!["first-account", "second-account"]);
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
                        provider: "openrouter".into(),
                        model: "z-ai/glm-5.2".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-sonnet-4-5".into(),
                        effort: None,
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
                    provider: "anthropic".into(),
                    model: "claude-sonnet-4-5".into(),
                    effort: None,
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
                        provider: "anthropic".into(),
                        model: "claude-first".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-second".into(),
                        effort: None,
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
                "model": "anthropic/claude-x",
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
        // Creation order (== priority order) now determines fallback order
        // within the family, since the route target is `{provider:
        // "anthropic", model}` rather than a connection id — "primary" is
        // added first so it's attempted first (and hits quota), leaving
        // "secondary" as the same-family backup.
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
        routes::save_model_route(
            &ctx.store,
            routes::ModelRouteInfo {
                id: "r1".into(),
                name: "task".into(),
                enabled: true,
                strategy: routes::ModelRouteStrategy::Fallback,
                targets: vec![routes::ModelRouteTarget {
                    provider: "anthropic".into(),
                    model: "claude-fable-5".into(),
                    effort: None,
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
    async fn anthropic_messages_pools_api_key_and_oauth_accounts_within_a_family() {
        // Cross-auth-method failover: an api-key `anthropic` account backs up
        // an `anthropic-oauth` account for the same model string, because
        // both share the "anthropic" family — this is the core of
        // family-aware routing (Task 3), not just same-provider fallback.
        use axum::{routing::post, Json, Router};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let oauth_hits = Arc::new(AtomicUsize::new(0));
        let oauth_hits_for_handler = oauth_hits.clone();

        async fn ok(Json(body): Json<Value>) -> Json<Value> {
            Json(json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": body["model"].clone(),
                "content": [{"type": "text", "text": "api-key backup worked"}],
                "usage": {"input_tokens": 1, "output_tokens": 2},
            }))
        }

        let app = Router::new()
            .route(
                "/first/messages",
                post(move || {
                    let hits = oauth_hits_for_handler.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        (
                            axum::http::StatusCode::TOO_MANY_REQUESTS,
                            Json(json!({"error": {"message": "You're out of extra usage."}})),
                        )
                    }
                }),
            )
            .route("/second/messages", post(ok));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        let now = crate::paths::now_ms();
        // Added first (lower priority) so it's attempted first and hits quota.
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "oauth-acct",
                "anthropic-oauth",
                "oauth",
                ConnectionData {
                    access_token: Some("sk-ant-oat-test".into()),
                    expires_at: Some(now + 24 * 60 * 60 * 1000),
                    last_refresh_at: Some(now),
                    base_url_override: Some(format!("http://127.0.0.1:{port}/first")),
                    models_override: Some(vec!["claude-test-1".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "apikey-acct",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-apikey".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}/second")),
                    models_override: Some(vec!["claude-test-1".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let response = anthropic_messages(
            &ctx,
            json!({
                "model": "anthropic/claude-test-1",
                "messages": [{"role": "user", "content": "hi"}],
            }),
        )
        .await
        .unwrap();

        assert_eq!(response["content"][0]["text"], "api-key backup worked");
        assert_eq!(
            oauth_hits.load(Ordering::SeqCst),
            1,
            "the anthropic-oauth account must have been tried (and hit quota) before falling back"
        );
    }

    #[tokio::test]
    // Test-only serialization of the process-wide JWT cache; the guard
    // legitimately spans awaits on the current_thread test runtime.
    #[allow(clippy::await_holding_lock)]
    async fn mimo_free_chat_request_carries_gate_headers_marker_and_bearer() {
        let _lock = crate::llm_router::mimo::test_cache_lock();
        crate::llm_router::mimo::store_jwt("chat-test-jwt");
        let ctx = test_ctx().await;
        let desc = registry::descriptor("mimo-free").unwrap();
        let target = RouteTarget {
            conn: mk_conn("m1", "mimo-free", "free", ConnectionData::default()),
            desc,
            upstream_model: "mimo-auto".into(),
            route_target_key: None,
        };
        let body = json!({
            "model": "mimo-auto",
            "messages": [{"role": "user", "content": "hi"}],
            "max_tokens": 8,
            "stream": false
        });
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://api.xiaomimimo.com/api/free-ai/openai/chat"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer chat-test-jwt"
        );
        assert!(req
            .headers()
            .get("user-agent")
            .unwrap()
            .to_str()
            .unwrap()
            .contains("Chrome"));
        assert_eq!(
            req.headers().get("x-mimo-source").unwrap(),
            "mimocode-cli-free"
        );
        assert!(req
            .headers()
            .get("x-session-affinity")
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with("ses_"));
        // Non-streaming request → Accept: application/json (stream-aware,
        // mirrors 9router executors/mimo-free.js).
        assert_eq!(req.headers().get("accept").unwrap(), "application/json");
        let sent: Value = serde_json::from_slice(req.body().unwrap().as_bytes().unwrap()).unwrap();
        assert_eq!(sent["messages"][0]["role"], "system");
        assert_eq!(
            sent["messages"][0]["content"],
            crate::llm_router::mimo::SYSTEM_MARKER
        );
        assert_eq!(sent["messages"][1]["content"], "hi");
        assert_eq!(sent["max_tokens"], 8);
        crate::llm_router::mimo::invalidate_jwt();
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
            route_target_key: None,
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
    // Test-only serialization of the process-wide JWT cache; the guard
    // legitimately spans awaits on the current_thread test runtime.
    #[allow(clippy::await_holding_lock)]
    async fn mimo_free_hits_nonstandard_chat_path_without_opencode_headers() {
        // With no cached bootstrap JWT, the request carries no bearer (it's
        // cache-driven, not hardcoded) and never opencode-free's headers.
        let _lock = crate::llm_router::mimo::test_cache_lock();
        crate::llm_router::mimo::invalidate_jwt();
        let ctx = test_ctx().await;
        let desc = registry::descriptor("mimo-free").unwrap();
        let conn = mk_conn("c6", "mimo-free", "free", ConnectionData::default());
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "mimo-auto".into(),
            route_target_key: None,
        };
        let body = json!({"model": "mimo-auto", "messages": []});
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://api.xiaomimimo.com/api/free-ai/openai/chat"
        );
        assert!(req.headers().get("authorization").is_none());
        assert!(req.headers().get("x-opencode-client").is_none());
    }

    #[tokio::test]
    async fn qwen_uses_resource_url_host_when_present() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("qwen").unwrap();
        let data = ConnectionData {
            access_token: Some("qtok".into()),
            provider_specific: Some(json!({ "resource_url": "dashscope.aliyuncs.com" })),
            ..Default::default()
        };
        let conn = mk_conn("q1", "qwen", "oauth", data);
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "qwen3-coder-plus".into(),
            route_target_key: None,
        };
        let body = json!({ "model": "qwen3-coder-plus", "messages": [] });
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://dashscope.aliyuncs.com/v1/chat/completions"
        );
        assert_eq!(req.headers().get("authorization").unwrap(), "Bearer qtok");
    }

    #[tokio::test]
    async fn qwen_falls_back_to_descriptor_base() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("qwen").unwrap();
        let conn = mk_conn(
            "q2",
            "qwen",
            "oauth",
            ConnectionData {
                access_token: Some("qtok".into()),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "qwen3-coder-plus".into(),
            route_target_key: None,
        };
        let body = json!({ "model": "qwen3-coder-plus", "messages": [] });
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://portal.qwen.ai/v1/chat/completions"
        );
    }

    #[tokio::test]
    async fn github_copilot_sends_bearer_and_mandatory_headers() {
        let ctx = test_ctx().await;
        let desc = registry::descriptor("github-copilot").unwrap();
        let conn = mk_conn(
            "g1",
            "github-copilot",
            "oauth",
            ConnectionData {
                access_token: Some("cop-tok".into()),
                ..Default::default()
            },
        );
        let target = RouteTarget {
            conn,
            desc,
            upstream_model: "gpt-5.2".into(),
            route_target_key: None,
        };
        let body = json!({ "model": "gpt-5.2", "messages": [] });
        let req = upstream_request(&ctx, &target, &body)
            .unwrap()
            .build()
            .unwrap();
        assert_eq!(
            req.url().as_str(),
            "https://api.githubcopilot.com/chat/completions"
        );
        assert_eq!(
            req.headers().get("authorization").unwrap(),
            "Bearer cop-tok"
        );
        assert_eq!(
            req.headers().get("copilot-integration-id").unwrap(),
            "vscode-chat"
        );
        assert_eq!(
            req.headers().get("editor-version").unwrap(),
            "vscode/1.110.0"
        );
        assert_eq!(
            req.headers().get("x-github-api-version").unwrap(),
            "2025-04-01"
        );
        assert!(req.headers().get("x-request-id").is_some());
    }

    #[test]
    fn copilot_sanitizer_serializes_unsupported_content_parts() {
        let mut body = json!({
            "model": "gpt-5.2",
            "messages": [
                { "role": "user", "content": "hi" },
                { "role": "user", "content": [
                    { "type": "text", "text": "keep me" },
                    { "type": "image_url", "image_url": { "url": "data:..." } },
                    { "type": "thinking", "thinking": "drop-to-text" }
                ]}
            ]
        });
        sanitize_copilot_body(&mut body);
        let parts = body["messages"][1]["content"].as_array().unwrap();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0]["type"], "text");
        assert_eq!(parts[1]["type"], "image_url");
        // The unsupported part is now a text part (serialized).
        assert_eq!(parts[2]["type"], "text");
        assert!(parts[2]["text"].as_str().unwrap().contains("thinking"));
        // String content is untouched.
        assert_eq!(body["messages"][0]["content"], "hi");
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
            route_target_key: None,
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
                output_tokens: 7,
                input_tokens: None,
                cache_read_tokens: None,
                cache_creation_tokens: None,
            })
        );
        let stop = ("message_stop".to_string(), json!({"type":"message_stop"}));
        assert_eq!(
            MessageStreamEvent::from_event(&stop),
            Some(MessageStreamEvent::MessageStop)
        );
    }

    #[tokio::test]
    async fn pinned_family_exhaustion_continues_down_matching_route() {
        use axum::{routing::post, Json, Router};
        use std::sync::atomic::{AtomicUsize, Ordering};

        let quota_hits = Arc::new(AtomicUsize::new(0));
        let quota_hits_handler = quota_hits.clone();

        async fn ok(Json(body): Json<Value>) -> Json<Value> {
            Json(json!({
                "id": "msg_1",
                "type": "message",
                "role": "assistant",
                "model": body["model"].clone(),
                "content": [{"type": "text", "text": "route continuation worked"}],
                "usage": {"input_tokens": 1, "output_tokens": 2},
            }))
        }

        let app = Router::new()
            .route(
                "/quota/messages",
                post(move || {
                    let hits = quota_hits_handler.clone();
                    async move {
                        hits.fetch_add(1, Ordering::SeqCst);
                        (
                            axum::http::StatusCode::TOO_MANY_REQUESTS,
                            Json(json!({"error": {"message": "You're out of extra usage."}})),
                        )
                    }
                }),
            )
            .route("/backup/messages", post(ok));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        // The only account serving the pinned model — quota'd out.
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "pinned-acct",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-a".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}/quota")),
                    models_override: Some(vec!["claude-a".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        // Serves only the route's second target model.
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "backup-acct",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-b".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}/backup")),
                    models_override: Some(vec!["claude-b".into()]),
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
                        provider: "anthropic".into(),
                        model: "claude-a".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-b".into(),
                        effort: None,
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
                "model": "anthropic/claude-a",
                "messages": [{"role": "user", "content": "hi"}],
            }),
        )
        .await
        .unwrap();

        assert_eq!(response["content"][0]["text"], "route continuation worked");
        assert_eq!(response["model"], "claude-b");
        // The (pinned-acct, claude-a) pair was already attempted in the
        // family pass — the route continuation must not retry it.
        assert_eq!(quota_hits.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn stream_continues_down_route_after_family_exhaustion() {
        use axum::{routing::post, Json, Router};

        let app = Router::new()
            .route(
                "/quota/messages",
                post(|| async {
                    (
                        axum::http::StatusCode::TOO_MANY_REQUESTS,
                        Json(json!({"error": {"message": "You're out of extra usage."}})),
                    )
                }),
            )
            .route(
                "/backup/messages",
                post(|| async { ([("content-type", "text/event-stream")], SSE_OK_STREAM) }),
            );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "pinned-acct",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-a".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}/quota")),
                    models_override: Some(vec!["claude-a".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "backup-acct",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-b".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}/backup")),
                    models_override: Some(vec!["claude-b".into()]),
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
                        provider: "anthropic".into(),
                        model: "claude-a".into(),
                        effort: None,
                    },
                    routes::ModelRouteTarget {
                        provider: "anthropic".into(),
                        model: "claude-b".into(),
                        effort: None,
                    },
                ],
                created_at: 1,
                updated_at: 1,
            },
        )
        .await
        .unwrap();

        let rx = anthropic_messages_stream(
            &ctx,
            json!({
                "model": "anthropic/claude-a",
                "messages": [{"role": "user", "content": "hi"}],
            }),
            utility_policy(&ctx, "anthropic/claude-a").await.as_ref(),
        )
        .await
        .unwrap();
        let events = collect_stream(rx.events).await;

        assert_eq!(stream_text(&events), "rotated");
    }

    #[tokio::test]
    async fn pinned_family_exhaustion_without_matching_route_surfaces_failures() {
        use axum::{routing::post, Json, Router};

        let app = Router::new().route(
            "/messages",
            post(|| async {
                (
                    axum::http::StatusCode::TOO_MANY_REQUESTS,
                    Json(json!({"error": {"message": "You're out of extra usage."}})),
                )
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let ctx = test_ctx().await;
        connections::add_connection(
            &ctx.store,
            mk_conn(
                "only-acct",
                "anthropic",
                "api_key",
                ConnectionData {
                    api_key: Some("sk-a".into()),
                    base_url_override: Some(format!("http://127.0.0.1:{port}")),
                    models_override: Some(vec!["claude-a".into()]),
                    ..Default::default()
                },
            ),
        )
        .await
        .unwrap();

        let err = anthropic_messages(
            &ctx,
            json!({
                "model": "anthropic/claude-a",
                "messages": [{"role": "user", "content": "hi"}],
            }),
        )
        .await
        .unwrap_err();

        assert!(err.to_string().contains("out of extra usage"), "got: {err}");
    }

    #[test]
    fn message_delta_decodes_input_and_cache_usage() {
        let ev = (
            "message_delta".to_string(),
            json!({"type":"message_delta","delta":{"stop_reason":"end_turn"},
                   "usage":{"output_tokens":7,"input_tokens":1200,
                            "cache_read_input_tokens":900,"cache_creation_input_tokens":0}}),
        );
        assert_eq!(
            MessageStreamEvent::from_event(&ev),
            Some(MessageStreamEvent::MessageDelta {
                stop_reason: Some("end_turn".into()),
                output_tokens: 7,
                input_tokens: Some(1200),
                cache_read_tokens: Some(900),
                cache_creation_tokens: None, // 0 filters to None
            })
        );
    }
}
