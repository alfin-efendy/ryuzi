//! `ryuzi:http/http` host adapter: the only network egress a component
//! plugin may use. Every request is checked against a per-plugin host
//! allowlist (built upstream from the plugin's manifest `permissions.network`
//! entries — see `runtime::CapabilityState::network_allowlist`), redirects
//! are never auto-followed by the underlying HTTP client so each hop can be
//! re-checked against the same allowlist, and any `Authorization` header a
//! component tries to supply is stripped before the request ever leaves the
//! host (Task 8 slice 2 — host-managed OAuth — is what injects real
//! authentication, and it must never be something a component can forge or
//! override).

/// Maximum number of redirect hops [`AllowedHttpClient::request`] will
/// follow manually before giving up. Bounded to avoid a malicious or
/// misbehaving allowlisted server driving the host into an unbounded loop.
const MAX_REDIRECT_HOPS: u8 = 5;

/// Header names the host strips from (or refuses to forward on) every
/// outbound request, regardless of what a component supplies.
///
/// - `authorization`: a component must never set its own auth — see the
///   module doc.
/// - `host`/`content-length`: managed by the HTTP client itself; forwarding
///   a component-supplied value could desync the request from its actual
///   body/target.
const STRIPPED_REQUEST_HEADERS: &[&str] = &["authorization", "host", "content-length"];

/// A capability-adapter-local error, mapped to the generated WIT
/// `http::HttpError` by the runtime's `Host` trait impl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HttpErr {
    InvalidRequest(String),
    Rejected,
    Unavailable,
    Failed(String),
}

/// An adapter-local response: status, headers, and raw body bytes, mapped to
/// the generated WIT `http::HttpResponse` by the runtime's `Host` trait
/// impl.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SafeHttpResponse {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

/// An HTTP client scoped to one plugin's declared network allowlist. Every
/// request — and every redirect hop a response chains through — is checked
/// against [`Self::host_is_allowed`] before it is sent.
pub struct AllowedHttpClient {
    allowlist: Vec<String>,
    http: reqwest::Client,
}

impl AllowedHttpClient {
    /// Builds a client scoped to `allowlist` (each entry either a bare
    /// hostname or a `*.`-prefixed wildcard — see [`Self::host_is_allowed`]).
    /// Entries are lowercased on construction so matching is
    /// case-insensitive without repeating the lowercase call per request.
    ///
    /// Automatic redirect-following is disabled
    /// (`redirect::Policy::none()`): the host itself walks each redirect hop
    /// so it can re-check the target host against `allowlist` before
    /// following it — `reqwest`'s built-in redirect handling has no way to
    /// veto a hop mid-chain.
    pub fn new(allowlist: Vec<String>) -> Self {
        let allowlist = allowlist.into_iter().map(|h| h.to_lowercase()).collect();
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("reqwest client with no non-default TLS/proxy config should always build");
        Self { allowlist, http }
    }

    /// `true` if `host` is covered by this client's allowlist. Matching is
    /// case-insensitive (`host` is lowercased before comparison, so callers
    /// need not pre-normalize it). An entry is either:
    ///
    /// - a bare hostname, matched by exact (case-insensitive) equality; or
    /// - a `*.`-prefixed wildcard (`*.github.com`), matched by any host that
    ///   ends with `.github.com` *and* has at least one more label before
    ///   it — i.e. `api.github.com` and `x.y.github.com` match, but the
    ///   apex `github.com` does not (a wildcard never implies its own
    ///   apex), and `evilgithub.com` does not (it ends with `github.com`
    ///   but not with `.github.com`, so the required label boundary is
    ///   absent).
    pub fn host_is_allowed(&self, host: &str) -> bool {
        let host = host.to_lowercase();
        self.allowlist.iter().any(|entry| {
            if let Some(suffix) = entry.strip_prefix("*.") {
                let dotted_suffix = format!(".{suffix}");
                host.ends_with(&dotted_suffix)
            } else {
                host == *entry
            }
        })
    }

    /// Issues one HTTP request, enforcing the allowlist on the initial
    /// target and on every redirect hop, stripping component-forged
    /// `Authorization`/`Host`/`Content-Length` headers, and returning either
    /// the final response or an [`HttpErr`].
    ///
    /// Redirect handling: because the underlying client never follows
    /// redirects itself (see [`Self::new`]), a `3xx` response with a
    /// `Location` header is resolved into an absolute URL against the
    /// request that produced it, its host is checked against the allowlist
    /// (rejecting the whole request if not covered), and — if allowed — the
    /// hop is re-issued as a fresh `GET` with no body and no headers at all
    /// (the component's original headers are dropped on every redirect so a
    /// forged `Authorization` or a body can never survive a hop). Note this
    /// is a deliberate, security-motivated narrowing: unlike RFC 7231, a
    /// `307`/`308` (which normally preserve method and body) is downgraded to
    /// a bodyless `GET` here too, so a POST body or component-supplied header
    /// is never resent to a subsequent hop. This repeats up to
    /// [`MAX_REDIRECT_HOPS`] times before giving up.
    pub async fn request(
        &self,
        method: &str,
        url: &str,
        headers: Vec<(String, String)>,
        body: Option<Vec<u8>>,
    ) -> Result<SafeHttpResponse, HttpErr> {
        let mut current_url =
            url::Url::parse(url).map_err(|error| HttpErr::InvalidRequest(error.to_string()))?;
        let mut current_method = method.to_string();
        let mut current_body = body;
        let mut current_headers = headers;

        for hop in 0..=MAX_REDIRECT_HOPS {
            let host = current_url
                .host_str()
                .ok_or_else(|| HttpErr::InvalidRequest("url has no host".to_string()))?;
            if !self.host_is_allowed(host) {
                return Err(HttpErr::Rejected);
            }

            let reqwest_method = reqwest::Method::from_bytes(current_method.as_bytes())
                .map_err(|error| HttpErr::InvalidRequest(error.to_string()))?;
            let mut builder = self.http.request(reqwest_method, current_url.clone());
            for (name, value) in &current_headers {
                if STRIPPED_REQUEST_HEADERS.contains(&name.to_lowercase().as_str()) {
                    continue;
                }
                builder = builder.header(name, value);
            }
            if let Some(bytes) = current_body.clone() {
                builder = builder.body(bytes);
            }

            let response = builder
                .send()
                .await
                .map_err(|error| HttpErr::Failed(error.to_string()))?;

            let status = response.status();
            if status.is_redirection() {
                if hop == MAX_REDIRECT_HOPS {
                    return Err(HttpErr::Failed("too many redirects".to_string()));
                }
                let Some(location) = response
                    .headers()
                    .get(reqwest::header::LOCATION)
                    .and_then(|value| value.to_str().ok())
                else {
                    // A 3xx with no usable Location is not a redirect the
                    // host can follow — return it to the caller as-is.
                    return build_response(response).await;
                };
                let next_url = current_url
                    .join(location)
                    .map_err(|error| HttpErr::Failed(error.to_string()))?;
                current_url = next_url;
                current_method = "GET".to_string();
                current_body = None;
                current_headers = Vec::new();
                continue;
            }

            return build_response(response).await;
        }

        // Defensively unreachable: every iteration of the loop above
        // returns via one of its branches within MAX_REDIRECT_HOPS + 1
        // iterations, so this fallback is never hit in practice.
        Err(HttpErr::Failed("redirect loop exceeded".to_string()))
    }
}

/// Builds a [`SafeHttpResponse`] from a terminal (non-redirected, or
/// unfollowable) `reqwest::Response`.
async fn build_response(response: reqwest::Response) -> Result<SafeHttpResponse, HttpErr> {
    let status = response.status().as_u16();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| {
            (
                name.as_str().to_string(),
                value.to_str().unwrap_or_default().to_string(),
            )
        })
        .collect();
    let body = response
        .bytes()
        .await
        .map_err(|error| HttpErr::Failed(error.to_string()))?
        .to_vec();
    Ok(SafeHttpResponse {
        status,
        headers,
        body,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::extract::Query;
    use axum::http::HeaderMap;
    use axum::response::{IntoResponse, Redirect};
    use axum::routing::get;
    use axum::Router;
    use std::collections::HashMap;

    /// Binds a fresh loopback listener and serves `app` in the background,
    /// returning the bound port. Mirrors the pattern used by
    /// `crates/core/tests/oauth_flow.rs` / `kiro_device.rs`.
    async fn spawn_server(app: Router) -> u16 {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("loopback listener should bind");
        let port = listener.local_addr().unwrap().port();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        port
    }

    #[tokio::test]
    async fn allowed_host_request_succeeds() {
        let app = Router::new().route("/ok", get(|| async { "hello from plugin server" }));
        let port = spawn_server(app).await;

        let client = AllowedHttpClient::new(vec!["127.0.0.1".to_string()]);
        let response = client
            .request("GET", &format!("http://127.0.0.1:{port}/ok"), vec![], None)
            .await
            .expect("allowlisted host must be permitted");

        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"hello from plugin server");
    }

    #[tokio::test]
    async fn unlisted_host_is_refused() {
        let app = Router::new().route("/ok", get(|| async { "hello" }));
        let port = spawn_server(app).await;

        let client = AllowedHttpClient::new(vec!["example.com".to_string()]);
        let result = client
            .request("GET", &format!("http://127.0.0.1:{port}/ok"), vec![], None)
            .await;

        assert_eq!(result, Err(HttpErr::Rejected));
    }

    #[tokio::test]
    async fn empty_allowlist_refuses_every_host() {
        let app = Router::new().route("/ok", get(|| async { "hello" }));
        let port = spawn_server(app).await;

        let client = AllowedHttpClient::new(vec![]);
        let result = client
            .request("GET", &format!("http://127.0.0.1:{port}/ok"), vec![], None)
            .await;

        assert_eq!(result, Err(HttpErr::Rejected));
    }

    #[tokio::test]
    async fn redirect_from_allowed_to_unlisted_host_is_refused() {
        let app = Router::new().route(
            "/start",
            get(|| async { Redirect::temporary("http://blocked.invalid/landing") }),
        );
        let port = spawn_server(app).await;

        // Only the redirect's origin host is allowlisted — the redirect
        // target (`blocked.invalid`) deliberately is not.
        let client = AllowedHttpClient::new(vec!["127.0.0.1".to_string()]);
        let result = client
            .request(
                "GET",
                &format!("http://127.0.0.1:{port}/start"),
                vec![],
                None,
            )
            .await;

        assert_eq!(result, Err(HttpErr::Rejected));
    }

    #[tokio::test]
    async fn redirect_to_an_allowed_host_is_followed() {
        let app = Router::new()
            .route("/start", get(|| async { Redirect::temporary("/landed") }))
            .route("/landed", get(|| async { "arrived" }));
        let port = spawn_server(app).await;

        let client = AllowedHttpClient::new(vec!["127.0.0.1".to_string()]);
        let response = client
            .request(
                "GET",
                &format!("http://127.0.0.1:{port}/start"),
                vec![],
                None,
            )
            .await
            .expect("redirect to an allowlisted host must be followed");

        assert_eq!(response.status, 200);
        assert_eq!(response.body, b"arrived");
    }

    #[tokio::test]
    async fn component_supplied_authorization_header_is_stripped() {
        async fn echo_auth_seen(headers: HeaderMap) -> impl IntoResponse {
            let seen = headers.contains_key(axum::http::header::AUTHORIZATION);
            if seen {
                "saw-auth"
            } else {
                "no-auth"
            }
        }
        let app = Router::new().route("/echo", get(echo_auth_seen));
        let port = spawn_server(app).await;

        let client = AllowedHttpClient::new(vec!["127.0.0.1".to_string()]);
        let response = client
            .request(
                "GET",
                &format!("http://127.0.0.1:{port}/echo"),
                vec![("Authorization".to_string(), "Bearer sneaky".to_string())],
                None,
            )
            .await
            .expect("request must still succeed once the header is stripped");

        assert_eq!(response.body, b"no-auth");
    }

    #[tokio::test]
    async fn other_headers_pass_through_unstripped() {
        async fn echo_header(
            headers: HeaderMap,
            Query(params): Query<HashMap<String, String>>,
        ) -> impl IntoResponse {
            let _ = params;
            headers
                .get("x-plugin-header")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("missing")
                .to_string()
        }
        let app = Router::new().route("/echo", get(echo_header));
        let port = spawn_server(app).await;

        let client = AllowedHttpClient::new(vec!["127.0.0.1".to_string()]);
        let response = client
            .request(
                "GET",
                &format!("http://127.0.0.1:{port}/echo"),
                vec![("x-plugin-header".to_string(), "keep-me".to_string())],
                None,
            )
            .await
            .expect("request should succeed");

        assert_eq!(response.body, b"keep-me");
    }

    #[test]
    fn wildcard_matches_any_subdomain_but_not_the_apex() {
        let client = AllowedHttpClient::new(vec!["*.github.com".to_string()]);
        assert!(client.host_is_allowed("api.github.com"));
        assert!(client.host_is_allowed("x.y.github.com"));
        assert!(!client.host_is_allowed("github.com"));
    }

    #[test]
    fn wildcard_does_not_match_a_host_merely_ending_in_the_suffix() {
        let client = AllowedHttpClient::new(vec!["*.github.com".to_string()]);
        assert!(!client.host_is_allowed("evilgithub.com"));
    }

    #[test]
    fn bare_entry_matches_by_exact_host_only() {
        let client = AllowedHttpClient::new(vec!["api.github.com".to_string()]);
        assert!(client.host_is_allowed("api.github.com"));
        assert!(!client.host_is_allowed("other.github.com"));
        assert!(!client.host_is_allowed("github.com"));
    }

    #[test]
    fn matching_is_case_insensitive() {
        let client = AllowedHttpClient::new(vec!["*.GitHub.com".to_string()]);
        assert!(client.host_is_allowed("API.GitHub.com"));
        assert!(client.host_is_allowed("api.github.com"));

        let exact = AllowedHttpClient::new(vec!["API.GitHub.com".to_string()]);
        assert!(exact.host_is_allowed("api.github.com"));
    }
}
