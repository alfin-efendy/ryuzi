//! Plugin OAuth helpers shared by the plugin host and UI-facing flows.

use base64::Engine;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const REFRESH_WINDOW_MS: i64 = 5 * 60 * 1000;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PluginOauthToken {
    pub plugin_id: String,
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub token_type: String,
    pub expires_at: Option<i64>,
    pub scopes: Vec<String>,
    pub reconnect_required: bool,
}

fn random_32() -> [u8; 32] {
    let mut bytes = [0u8; 32];
    bytes[..16].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes[16..].copy_from_slice(uuid::Uuid::new_v4().as_bytes());
    bytes
}

fn b64url(bytes: &[u8]) -> String {
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

pub fn generate_pkce_verifier() -> String {
    b64url(&random_32())
}

pub fn pkce_challenge_s256(verifier: &str) -> String {
    b64url(&Sha256::digest(verifier.as_bytes()))
}

pub fn needs_refresh(now: i64, expires_at: Option<i64>) -> bool {
    match expires_at {
        Some(expires_at) => expires_at - now <= REFRESH_WINDOW_MS,
        None => true,
    }
}

pub fn parse_www_authenticate_resource(header: &str) -> Option<String> {
    let mut resource_metadata = None;
    let mut resource = None;

    for (key, value) in parse_www_authenticate_params(header) {
        match key.as_str() {
            "resource_metadata" if resource_metadata.is_none() => resource_metadata = Some(value),
            "resource" if resource.is_none() => resource = Some(value),
            _ => {}
        }
    }

    resource_metadata.or(resource)
}

fn parse_www_authenticate_params(header: &str) -> Vec<(String, String)> {
    let bytes = header.as_bytes();
    let mut pairs = Vec::new();
    let mut i = 0usize;

    while i < bytes.len() {
        while i < bytes.len() && (bytes[i].is_ascii_whitespace() || bytes[i] == b',') {
            i += 1;
        }
        if i >= bytes.len() {
            break;
        }

        let key_start = i;
        while i < bytes.len()
            && !bytes[i].is_ascii_whitespace()
            && bytes[i] != b'='
            && bytes[i] != b','
        {
            i += 1;
        }
        if key_start == i {
            i += 1;
            continue;
        }
        let key = header[key_start..i].trim().to_ascii_lowercase();
        let is_likely_scheme =
            !key.is_empty() && key.as_bytes().iter().all(|byte| byte.is_ascii_alphabetic());

        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() || bytes[i] != b'=' {
            if is_likely_scheme {
                if i < bytes.len() && bytes[i] == b',' {
                    i += 1;
                }
                while i < bytes.len() && bytes[i].is_ascii_whitespace() {
                    i += 1;
                }
                continue;
            }
            while i < bytes.len() && bytes[i] != b',' {
                i += 1;
            }
            continue;
        }
        i += 1;
        while i < bytes.len() && bytes[i].is_ascii_whitespace() {
            i += 1;
        }
        if i >= bytes.len() {
            pairs.push((key, String::new()));
            break;
        }

        let value = if bytes[i] == b'"' {
            i += 1;
            let mut value = String::new();
            while i < bytes.len() {
                match bytes[i] {
                    b'\\' if i + 1 < bytes.len() => {
                        value.push(bytes[i + 1] as char);
                        i += 2;
                    }
                    b'"' => {
                        i += 1;
                        break;
                    }
                    byte => {
                        value.push(byte as char);
                        i += 1;
                    }
                }
            }
            value
        } else {
            let value_start = i;
            while i < bytes.len() && bytes[i] != b',' {
                i += 1;
            }
            header[value_start..i].trim().to_string()
        };
        pairs.push((key, value));
    }

    pairs
}

/// RFC 8414 authorization-server metadata — only the fields we need.
#[derive(Debug, Clone, Deserialize)]
pub struct OauthServerMetadata {
    pub authorization_endpoint: String,
    pub token_endpoint: String,
    #[serde(default)]
    pub registration_endpoint: Option<String>,
}

/// The RFC 8414 discovery URL candidates for `resource`, most specific
/// first: the §3 path-inserted form (only when the resource URL has a
/// path), then the origin-root form.
fn discovery_urls(resource: &str) -> anyhow::Result<Vec<String>> {
    let url = url::Url::parse(resource)?;
    let origin = url.origin().ascii_serialization();
    let path = url.path().trim_end_matches('/');
    let mut candidates = Vec::new();
    if !path.is_empty() && path != "/" {
        candidates.push(format!(
            "{origin}/.well-known/oauth-authorization-server{path}"
        ));
    }
    candidates.push(format!("{origin}/.well-known/oauth-authorization-server"));
    Ok(candidates)
}

/// Fetch the RFC 8414 document for `resource` (the manifest's full
/// `auth.resource` URL). Discovery order: path-inserted form first when the
/// resource has a path, then origin-root. First 2xx-with-valid-JSON wins;
/// every candidate failing is a discovery error naming the last failure.
pub async fn discover_oauth_server_metadata(
    http: &reqwest::Client,
    resource: &str,
) -> anyhow::Result<OauthServerMetadata> {
    let mut last_failure = String::new();
    for candidate in discovery_urls(resource)? {
        match http.get(&candidate).send().await {
            Ok(response) if response.status().is_success() => {
                match response.json::<OauthServerMetadata>().await {
                    Ok(metadata) => return Ok(metadata),
                    Err(err) => last_failure = format!("{candidate}: invalid metadata: {err}"),
                }
            }
            Ok(response) => last_failure = format!("{candidate}: HTTP {}", response.status()),
            Err(err) => last_failure = format!("{candidate}: {err}"),
        }
    }
    anyhow::bail!("OAuth discovery failed for {resource} — {last_failure}")
}

/// RFC 7591 dynamic-client-registration request. We always register as a
/// PUBLIC PKCE client — never a confidential one.
#[derive(Debug, Serialize)]
struct DcrRegistrationRequest {
    redirect_uris: Vec<String>,
    token_endpoint_auth_method: &'static str, // "none" — public PKCE client
    grant_types: Vec<&'static str>,           // ["authorization_code", "refresh_token"]
    response_types: Vec<&'static str>,        // ["code"]
    client_name: &'static str,                // "Ryuzi"
}

/// NOTE: if the server also returns `client_secret`, serde skips it (no
/// deny_unknown_fields) and we deliberately IGNORE it — we registered as a
/// public PKCE client, so `plugin_oauth_clients` never stores a secret and
/// needs no encryption.
#[derive(Debug, Deserialize)]
struct DcrRegistrationResponse {
    client_id: String,
}

/// RFC 7591 registration against `registration_endpoint`. Returns the new
/// client_id.
pub async fn register_oauth_client(
    http: &reqwest::Client,
    registration_endpoint: &str,
    redirect_uri: &str,
) -> anyhow::Result<String> {
    let request = DcrRegistrationRequest {
        redirect_uris: vec![redirect_uri.to_string()],
        token_endpoint_auth_method: "none",
        grant_types: vec!["authorization_code", "refresh_token"],
        response_types: vec!["code"],
        client_name: "Ryuzi",
    };
    let response = http
        .post(registration_endpoint)
        .json(&request)
        .send()
        .await?;
    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        let detail = body.trim();
        if detail.is_empty() {
            anyhow::bail!("dynamic client registration failed with HTTP {status}");
        }
        anyhow::bail!("dynamic client registration failed with HTTP {status}: {detail}");
    }
    let payload: DcrRegistrationResponse = response.json().await?;
    if payload.client_id.is_empty() {
        anyhow::bail!("dynamic client registration returned an empty client_id");
    }
    Ok(payload.client_id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pkce_challenge_matches_the_rfc7636_s256_example() {
        assert_eq!(
            pkce_challenge_s256("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"),
            "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"
        );
    }

    #[test]
    fn generated_pkce_verifier_is_urlsafe_and_decodes_to_32_bytes() {
        let verifier = generate_pkce_verifier();
        let decoded = base64::engine::general_purpose::URL_SAFE_NO_PAD
            .decode(&verifier)
            .unwrap();
        assert_eq!(decoded.len(), 32);
        assert!(!verifier.contains('=') && !verifier.contains('+') && !verifier.contains('/'));
    }

    #[test]
    fn needs_refresh_treats_missing_expiry_as_due_now() {
        assert!(needs_refresh(1_700_000_000_000, None));
    }

    #[test]
    fn needs_refresh_flips_true_inside_the_five_minute_window() {
        let now = 1_700_000_000_000;
        assert!(!needs_refresh(now, Some(now + REFRESH_WINDOW_MS + 1)));
        assert!(needs_refresh(now, Some(now + REFRESH_WINDOW_MS)));
        assert!(needs_refresh(now, Some(now - 1)));
    }

    #[test]
    fn parse_www_authenticate_prefers_resource_metadata_over_resource() {
        let header = r#"Bearer realm="mcp", resource="https://api.example.test", resource_metadata="https://api.example.test/.well-known/oauth-protected-resource""#;
        assert_eq!(
            parse_www_authenticate_resource(header).as_deref(),
            Some("https://api.example.test/.well-known/oauth-protected-resource")
        );
    }

    #[test]
    fn parse_www_authenticate_prefers_resource_metadata_when_prefixed_by_bearer() {
        let header = r#"Bearer resource_metadata="https://api.example.test/.well-known/oauth-protected-resource""#;
        assert_eq!(
            parse_www_authenticate_resource(header).as_deref(),
            Some("https://api.example.test/.well-known/oauth-protected-resource")
        );
    }

    #[test]
    fn parse_www_authenticate_reads_unprefixed_resource() {
        let header = r#"Bearer resource="https://api.example.test""#;
        assert_eq!(
            parse_www_authenticate_resource(header).as_deref(),
            Some("https://api.example.test")
        );
    }

    #[test]
    fn parse_www_authenticate_reads_bearer_resource_after_another_challenge() {
        let header = r#"Basic realm="x", Bearer resource="https://api.example.test""#;
        assert_eq!(
            parse_www_authenticate_resource(header).as_deref(),
            Some("https://api.example.test")
        );
    }

    #[test]
    fn parse_www_authenticate_accepts_unquoted_resource_values() {
        let header = "Bearer error=invalid_token, resource=https://api.example.test, scope=repo";
        assert_eq!(
            parse_www_authenticate_resource(header).as_deref(),
            Some("https://api.example.test")
        );
    }

    #[test]
    fn parse_www_authenticate_handles_quoted_commas_and_escapes() {
        let header = "Bearer title=\"repo, issues\", resource_metadata=\"https://example.test/.well-known/oauth-protected-resource?label=repo\\\"tools\"";
        assert_eq!(
            parse_www_authenticate_resource(header).as_deref(),
            Some("https://example.test/.well-known/oauth-protected-resource?label=repo\"tools")
        );
    }

    #[test]
    fn parse_www_authenticate_returns_none_when_no_resource_is_present() {
        assert_eq!(
            parse_www_authenticate_resource(r#"Bearer realm="mcp", error="invalid_token""#),
            None
        );
    }

    // ---------- RFC 8414 discovery + RFC 7591 DCR ----------

    async fn serve(app: axum::Router) -> String {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        base
    }

    #[test]
    fn discovery_urls_orders_path_inserted_before_root() {
        assert_eq!(
            discovery_urls("https://mcp.atlassian.com/v1/mcp/authv2").unwrap(),
            vec![
                "https://mcp.atlassian.com/.well-known/oauth-authorization-server/v1/mcp/authv2"
                    .to_string(),
                "https://mcp.atlassian.com/.well-known/oauth-authorization-server".to_string(),
            ]
        );
        assert_eq!(
            discovery_urls("https://mcp.vercel.com").unwrap(),
            vec!["https://mcp.vercel.com/.well-known/oauth-authorization-server".to_string()]
        );
    }

    #[tokio::test]
    async fn discovery_root_form_succeeds_for_a_pathless_resource() {
        use axum::{routing::get, Json, Router};
        let app = Router::new().route(
            "/.well-known/oauth-authorization-server",
            get(|| async {
                Json(serde_json::json!({
                    "authorization_endpoint": "https://vendor.test/authorize",
                    "token_endpoint": "https://vendor.test/token",
                    "registration_endpoint": "https://vendor.test/register"
                }))
            }),
        );
        let base = serve(app).await;
        let http = reqwest::Client::new();
        let metadata = discover_oauth_server_metadata(&http, &base).await.unwrap();
        assert_eq!(metadata.authorization_endpoint, "https://vendor.test/authorize");
        assert_eq!(metadata.token_endpoint, "https://vendor.test/token");
        assert_eq!(
            metadata.registration_endpoint.as_deref(),
            Some("https://vendor.test/register")
        );
    }

    #[tokio::test]
    async fn discovery_path_inserted_only_vendor_is_found_first() {
        use axum::{routing::get, Json, Router};
        // Atlassian shape: ONLY the RFC 8414 §3 path-inserted document
        // exists; the origin-root form 404s (axum default).
        let app = Router::new().route(
            "/.well-known/oauth-authorization-server/v1/mcp/authv2",
            get(|| async {
                Json(serde_json::json!({
                    "authorization_endpoint": "https://vendor.test/path-authorize",
                    "token_endpoint": "https://vendor.test/path-token"
                }))
            }),
        );
        let base = serve(app).await;
        let http = reqwest::Client::new();
        let metadata = discover_oauth_server_metadata(&http, &format!("{base}/v1/mcp/authv2"))
            .await
            .unwrap();
        assert_eq!(metadata.authorization_endpoint, "https://vendor.test/path-authorize");
        assert!(metadata.registration_endpoint.is_none());
    }

    #[tokio::test]
    async fn discovery_fails_when_both_forms_404() {
        use axum::Router;
        let base = serve(Router::new()).await;
        let http = reqwest::Client::new();
        let err = discover_oauth_server_metadata(&http, &format!("{base}/mcp"))
            .await
            .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("OAuth discovery failed"), "{msg}");
        assert!(msg.contains("404"), "{msg}");
    }

    #[tokio::test]
    async fn dcr_registers_a_public_pkce_client_and_ignores_client_secret() {
        use axum::{routing::post, Json, Router};
        let app = Router::new().route(
            "/register",
            post(|Json(body): Json<serde_json::Value>| async move {
                assert_eq!(body["token_endpoint_auth_method"], "none");
                assert_eq!(body["client_name"], "Ryuzi");
                assert_eq!(
                    body["grant_types"],
                    serde_json::json!(["authorization_code", "refresh_token"])
                );
                assert_eq!(body["response_types"], serde_json::json!(["code"]));
                assert_eq!(
                    body["redirect_uris"],
                    serde_json::json!(["http://127.0.0.1:8976/plugin-oauth/acme/callback"])
                );
                Json(serde_json::json!({
                    "client_id": "dcr-client-1",
                    "client_secret": "must-be-ignored"
                }))
            }),
        );
        let base = serve(app).await;
        let http = reqwest::Client::new();
        let client_id = register_oauth_client(
            &http,
            &format!("{base}/register"),
            "http://127.0.0.1:8976/plugin-oauth/acme/callback",
        )
        .await
        .unwrap();
        assert_eq!(client_id, "dcr-client-1");
    }

    #[tokio::test]
    async fn dcr_rejection_is_an_error_carrying_the_body_detail() {
        use axum::{http::StatusCode, routing::post, Router};
        let app = Router::new().route(
            "/register",
            post(|| async { (StatusCode::FORBIDDEN, r#"{"error":"approved clients only"}"#) }),
        );
        let base = serve(app).await;
        let http = reqwest::Client::new();
        let err = register_oauth_client(
            &http,
            &format!("{base}/register"),
            "http://127.0.0.1:8976/plugin-oauth/acme/callback",
        )
        .await
        .unwrap_err();
        let msg = err.to_string();
        assert!(msg.contains("dynamic client registration failed"), "{msg}");
        assert!(msg.contains("approved clients only"), "{msg}");
    }
}
