//! Integration-level coverage of the Kiro AWS SSO-OIDC device-code flow and
//! the Kiro-IDE-cache import path, driven end to end against mock HTTP
//! servers / a fixture directory. `oauth::device` and `oauth::import` already
//! have thorough `#[cfg(test)]` unit coverage inside `crates/core/src`; these
//! tests exercise the same public entry points from outside the crate, the
//! way a real caller (the device-flow connect command, the IDE-import
//! command) would.
use ryuzi_core::llm_router::oauth::{device, import};
use ryuzi_core::llm_router::registry::KIRO_DEVICE_FLOW;
use serde_json::json;

/// Drive `register_client_at` -> `start_device_authorization_at` ->
/// `poll_token_once` (first poll `authorization_pending`, second `Ready`)
/// against one mock OIDC server exposing all three endpoints, and assert the
/// resulting `DeviceTokens` has a sane `expires_at`.
#[tokio::test]
async fn device_flow_end_to_end_register_authorize_and_poll_to_ready() {
    use axum::{routing::post, Json, Router};
    use std::sync::{Arc, Mutex};

    let poll_count = Arc::new(Mutex::new(0u32));
    let poll_count_for_handler = poll_count.clone();

    let app = Router::new()
        .route(
            "/client/register",
            post(|Json(b): Json<serde_json::Value>| async move {
                assert_eq!(b["clientType"], "public");
                Json(json!({"clientId": "client-xyz", "clientSecret": "secret-xyz"}))
            }),
        )
        .route(
            "/device_authorization",
            post(|Json(b): Json<serde_json::Value>| async move {
                assert_eq!(b["clientId"], "client-xyz");
                assert_eq!(b["clientSecret"], "secret-xyz");
                Json(json!({
                    "deviceCode": "dc-xyz",
                    "userCode": "ABCD-1234",
                    "verificationUri": "https://device.sso.us-east-1.amazonaws.com/",
                    "verificationUriComplete": "https://device.sso.us-east-1.amazonaws.com/?user_code=ABCD-1234",
                    "expiresIn": 600,
                    "interval": 1,
                }))
            }),
        )
        .route(
            "/token",
            post(move |Json(b): Json<serde_json::Value>| {
                let poll_count = poll_count_for_handler.clone();
                async move {
                    assert_eq!(b["deviceCode"], "dc-xyz");
                    let mut n = poll_count.lock().unwrap();
                    *n += 1;
                    if *n == 1 {
                        Json(json!({"error": "authorization_pending"}))
                    } else {
                        Json(json!({
                            "accessToken": "at-device",
                            "refreshToken": "rt-device",
                            "expiresIn": 3600,
                        }))
                    }
                }
            }),
        );

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });

    let http = reqwest::Client::new();
    let client = device::register_client_at(
        &http,
        &format!("http://127.0.0.1:{port}/client/register"),
        &KIRO_DEVICE_FLOW,
    )
    .await
    .unwrap();
    assert_eq!(client.client_id, "client-xyz");
    assert_eq!(client.client_secret, "secret-xyz");

    let auth = device::start_device_authorization_at(
        &http,
        &format!("http://127.0.0.1:{port}/device_authorization"),
        &KIRO_DEVICE_FLOW,
        &client,
    )
    .await
    .unwrap();
    assert_eq!(auth.device_code, "dc-xyz");
    assert_eq!(auth.user_code, "ABCD-1234");
    assert_eq!(auth.interval, 1);

    let token_url = format!("http://127.0.0.1:{port}/token");

    let first = device::poll_token_once(&http, &token_url, &client, &auth.device_code)
        .await
        .unwrap();
    assert_eq!(first, device::PollOutcome::Pending);

    let before = ryuzi_core::paths::now_ms();
    let second = device::poll_token_once(&http, &token_url, &client, &auth.device_code)
        .await
        .unwrap();
    match second {
        device::PollOutcome::Ready(tokens) => {
            assert_eq!(tokens.access_token, "at-device");
            assert_eq!(tokens.refresh_token.as_deref(), Some("rt-device"));
            let expected = before + 3600 * 1000;
            assert!(
                (tokens.expires_at - expected).abs() < 5_000,
                "expires_at {} should be close to {expected}",
                tokens.expires_at
            );
        }
        other => panic!("expected Ready, got {other:?}"),
    }
}

/// Build the Task-4 IDE-cache fixture shape (`kiro-auth-token.json` +
/// `{clientIdHash}.json` + `profile.json`) in a tempdir and assert
/// `read_kiro_ide_cache_from` returns the expected `ImportedKiro`.
#[test]
fn ide_cache_import_reads_task4_fixture_shape() {
    let base = std::env::temp_dir().join(format!(
        "ryuzi-kiro-device-fixture-{}",
        uuid::Uuid::new_v4()
    ));
    let sso_dir = base.join("sso-cache");
    std::fs::create_dir_all(&sso_dir).unwrap();
    std::fs::write(
        sso_dir.join("kiro-auth-token.json"),
        r#"{"refreshToken":"aorAAAAAGintegration","region":"us-east-1","authMethod":"idc","clientIdHash":"hash123"}"#,
    )
    .unwrap();
    std::fs::write(
        sso_dir.join("hash123.json"),
        r#"{"clientId":"cid-int","clientSecret":"csecret-int"}"#,
    )
    .unwrap();
    let profile_json = base.join("profile.json");
    std::fs::write(
        &profile_json,
        r#"{"arn":"arn:aws:codewhisperer:eu-west-1:999:profile/Integration"}"#,
    )
    .unwrap();

    let imported = import::read_kiro_ide_cache_from(&sso_dir, Some(&profile_json)).unwrap();

    assert_eq!(imported.refresh_token, "aorAAAAAGintegration");
    assert_eq!(imported.region.as_deref(), Some("us-east-1"));
    assert_eq!(imported.auth_method, "idc");
    assert_eq!(imported.client_id.as_deref(), Some("cid-int"));
    assert_eq!(imported.client_secret.as_deref(), Some("csecret-int"));
    assert_eq!(
        imported.profile_arn.as_deref(),
        Some("arn:aws:codewhisperer:us-east-1:999:profile/Integration")
    );

    std::fs::remove_dir_all(&base).ok();
}

/// The "imported" (no client registration, no profile.json) variant of the
/// fixture: no `clientIdHash` at all, so `auth_method` resolves to
/// `"imported"` and both `client_id`/`profile_arn` are `None`.
#[test]
fn ide_cache_import_falls_back_to_imported_auth_method_without_client_registration() {
    let base = std::env::temp_dir().join(format!(
        "ryuzi-kiro-device-fixture-imported-{}",
        uuid::Uuid::new_v4()
    ));
    let sso_dir = base.join("sso-cache");
    std::fs::create_dir_all(&sso_dir).unwrap();
    std::fs::write(
        sso_dir.join("kiro-auth-token.json"),
        r#"{"refreshToken":"aorAAAAAGnocreds","region":"eu-west-1"}"#,
    )
    .unwrap();

    let imported = import::read_kiro_ide_cache_from(&sso_dir, None).unwrap();

    assert_eq!(imported.refresh_token, "aorAAAAAGnocreds");
    assert_eq!(imported.region.as_deref(), Some("eu-west-1"));
    assert_eq!(imported.auth_method, "imported");
    assert_eq!(imported.client_id, None);
    assert_eq!(imported.client_secret, None);
    assert_eq!(imported.profile_arn, None);

    std::fs::remove_dir_all(&base).ok();
}
