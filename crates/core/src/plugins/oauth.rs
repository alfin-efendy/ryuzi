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
}
