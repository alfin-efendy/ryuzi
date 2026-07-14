//! End-to-end: a built daemon serving the control API — auth, rpc, SSE.

use ryuzi_core::daemon::{build_daemon, BuildDaemonOpts};
use ryuzi_core::domain::WriteOrigin;
use ryuzi_core::serve::{serve, ApiState, ServeOpts};
use ryuzi_core::store::Store;
use ryuzi_core::telemetry::NoopTelemetry;
use serde_json::json;
use std::net::Ipv4Addr;
use std::sync::Arc;

// ---- P2-9: pair -> device token -> authed rpc/sse over pinned TLS ----
//
// Everything above this line runs the control API in plaintext over
// loopback (today's Cockpit-local shape). This section proves the OTHER
// half of the remote-runner story end to end: a real ring `ServerConfig`
// (`tls::load_or_generate` + `tls::server_config`, same construction
// `resolve_bind` uses for a non-loopback bind), served over TLS, reached by
// a client that PINS the self-signed leaf certificate by fingerprint rather
// than trusting a CA chain — the exact trust model `tls.rs`'s module docs
// describe for Phase-3 remote clients, and the exact client shape Phase-3's
// Cockpit will use once it talks to a remote daemon it has already paired
// with.

/// A `rustls::client::danger::ServerCertVerifier` that trusts ONE
/// certificate: the one whose SHA-256 fingerprint (base64, standard
/// alphabet — computed via the real `tls::fingerprint_cert_der`, not a
/// reimplementation, so the two can never drift) matches
/// `expected_fingerprint`. This is TOFU certificate pinning, not
/// `danger_accept_invalid_certs` — a presented cert with the WRONG
/// fingerprint is rejected, `danger_accept_invalid_certs` would accept it.
///
/// Signature verification is NOT skipped: `verify_tls12_signature` /
/// `verify_tls13_signature` delegate to the ring provider's own algorithms
/// (`rustls::crypto::verify_tls12_signature` / `verify_tls13_signature`), so
/// this verifier only relaxes the CA-chain-of-trust check (there is no CA —
/// see `tls.rs`'s module docs), not cryptographic signature validation.
#[derive(Debug)]
struct FingerprintPin {
    expected_fingerprint: String,
    provider: Arc<rustls::crypto::CryptoProvider>,
}

impl rustls::client::danger::ServerCertVerifier for FingerprintPin {
    fn verify_server_cert(
        &self,
        end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp_response: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        let presented = ryuzi_core::tls::fingerprint_cert_der(end_entity.as_ref());
        if presented == self.expected_fingerprint {
            Ok(rustls::client::danger::ServerCertVerified::assertion())
        } else {
            Err(rustls::Error::General(format!(
                "fingerprint pin mismatch: expected {}, got {presented}",
                self.expected_fingerprint
            )))
        }
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.provider.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.provider
            .signature_verification_algorithms
            .supported_schemes()
    }
}

/// A `reqwest::Client` wired to a ring-provider `rustls::ClientConfig` whose
/// only trust decision is [`FingerprintPin`] — built via
/// `ClientBuilder::use_preconfigured_tls`, the supported way to hand reqwest
/// a fully custom rustls config (confirmed against the vendored reqwest
/// 0.12.28 / rustls 0.23.41 sources: `use_preconfigured_tls` downcasts to
/// `rustls::ClientConfig` and, when it matches, routes straight to
/// `Connector::new_rustls_tls`, bypassing reqwest's own root-store/verifier
/// setup entirely).
fn pinned_client(fingerprint: &str) -> reqwest::Client {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let verifier = Arc::new(FingerprintPin {
        expected_fingerprint: fingerprint.to_string(),
        provider: provider.clone(),
    });
    let client_config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("ring provider supports default protocol versions")
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_no_client_auth();

    reqwest::Client::builder()
        .use_preconfigured_tls(client_config)
        .build()
        .expect("reqwest client builds over the preconfigured pinned rustls config")
}

/// End-to-end: real TLS (ring `ServerConfig` from `tls::load_or_generate` +
/// `tls::server_config`) on loopback, reached by a client that pins the
/// self-signed leaf by fingerprint — mint a pairing code, redeem it over
/// `POST /pair` for a device token, then use that token to reach an authed
/// RPC route and open the SSE stream, all over the SAME pinned-TLS client.
#[tokio::test]
async fn remote_pair_then_authed_rpc_and_sse_over_pinned_tls() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("ryuzi.sqlite");

    let daemon = build_daemon(BuildDaemonOpts {
        db_path,
        config_root: tmp.path().to_path_buf(),
        telemetry: Some(Arc::new(NoopTelemetry)),
        extra_gateway_factories: vec![],
        harness_factory: None,
    })
    .await
    .unwrap();
    daemon.start().await.unwrap();

    let control_token = ryuzi_core::control_token::write_token(tmp.path()).unwrap();

    // Real ring TLS material + `ServerConfig` — same construction P2-7's
    // `tls::resolve_bind` uses for a non-loopback bind, persisted under the
    // same control dir a real daemon would use.
    let material = ryuzi_core::tls::load_or_generate(tmp.path()).unwrap();
    let tls_cfg = ryuzi_core::tls::server_config(&material).unwrap();

    let port = serve(
        ApiState {
            cp: daemon.cp.clone(),
            router_server: daemon.router_server.clone(),
            agents: daemon.agents.clone(),
            agent_knowledge: daemon.agent_knowledge.clone(),
            learning_queue: daemon.learning_queue.clone(),
            control_token,
        },
        ServeOpts {
            addr: Ipv4Addr::LOCALHOST.into(),
            port: 0,
            tls: Some(tls_cfg),
        },
    )
    .await
    .unwrap();
    let base = format!("https://127.0.0.1:{port}");
    let client = pinned_client(&material.fingerprint);

    // Mint a pairing code the same way `ryuzi pair` (P2-8) does, then redeem
    // it over the pinned-TLS client — the exact bootstrap path a brand-new
    // Phase-3 Cockpit device performs against a remote daemon.
    let store = daemon.cp.store().clone();
    let code = ryuzi_core::pairing::mint_code(&store, 60_000, ryuzi_core::paths::now_ms())
        .await
        .unwrap();

    let resp = client
        .post(format!("{base}/pair"))
        .json(&json!({ "code": code, "device_name": "pinned-tls-test-device" }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    let device_token = body["device_token"]
        .as_str()
        .expect("device_token present on a successful /pair")
        .to_string();

    // The freshly paired device token authenticates an authed RPC route —
    // over the pinned-TLS connection, proving the whole chain: TLS
    // handshake pinned by fingerprint -> /pair -> device token -> authed
    // /sessions.
    let resp = client
        .get(format!("{base}/sessions"))
        .bearer_auth(&device_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert!(
        body["sessions"].as_array().is_some(),
        "authed /sessions over pinned TLS returns the usual {{sessions: [...]}} envelope"
    );

    // GET /events over the same pinned client: the SSE connection itself
    // establishes (200 + text/event-stream) with the device token — proves
    // `/events` sits behind the same two-tier auth as every other authed
    // route even over TLS, not just over plaintext loopback (see the
    // plaintext SSE coverage above).
    let resp = client
        .get(format!("{base}/events"))
        .bearer_auth(&device_token)
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let content_type = resp
        .headers()
        .get(reqwest::header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    assert!(
        content_type.contains("text/event-stream"),
        "unexpected /events content-type over pinned TLS: {content_type:?}"
    );

    daemon.stop().await;
}

#[tokio::test]
async fn daemon_control_api_serves_rpc_and_sse_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("ryuzi.sqlite");

    let daemon = build_daemon(BuildDaemonOpts {
        db_path,
        config_root: tmp.path().to_path_buf(),
        telemetry: Some(Arc::new(NoopTelemetry)),
        extra_gateway_factories: vec![],
        harness_factory: None,
    })
    .await
    .unwrap();
    daemon.start().await.unwrap();

    let token = ryuzi_core::control_token::write_token(tmp.path()).unwrap();
    let port = serve(
        ApiState {
            cp: daemon.cp.clone(),
            router_server: daemon.router_server.clone(),
            agents: daemon.agents.clone(),
            agent_knowledge: daemon.agent_knowledge.clone(),
            learning_queue: daemon.learning_queue.clone(),
            control_token: token.clone(),
        },
        ServeOpts {
            addr: Ipv4Addr::LOCALHOST.into(),
            port: 0,
            tls: None,
        },
    )
    .await
    .unwrap();
    let base = format!("http://127.0.0.1:{port}");
    let client = reqwest::Client::new();

    // rpc round trip
    client
        .post(format!("{base}/rpc/set_setting"))
        .bearer_auth(&token)
        .json(&json!({"key": "control_smoke", "value": "1"}))
        .send()
        .await
        .unwrap()
        .error_for_status()
        .unwrap();
    let v: serde_json::Value = client
        .post(format!("{base}/rpc/get_setting"))
        .bearer_auth(&token)
        .json(&json!({"key": "control_smoke"}))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v, json!("1"));

    // SSE carries an emitted CoreEvent
    let cp = daemon.cp.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
        cp.emit(ryuzi_core::CoreEvent::Notice {
            session_pk: "s-x".into(),
            text: "hello-sse".into(),
        });
    });
    let resp = client
        .get(format!("{base}/events"))
        .bearer_auth(&token)
        .send()
        .await
        .unwrap();
    let body = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        use futures::StreamExt;
        let mut stream = resp.bytes_stream();
        let mut buf = String::new();
        while let Some(chunk) = stream.next().await {
            buf.push_str(&String::from_utf8_lossy(&chunk.unwrap()));
            if buf.contains("hello-sse") {
                return buf;
            }
        }
        buf
    })
    .await
    .unwrap();
    assert!(body.contains("hello-sse"));

    // ---- controller addition 1: /events without a token is 401 ----
    // The SSE route sits behind the same auth middleware as every other
    // `authed` route (see `serve::router`) — hit it with no Authorization
    // header and confirm the gate actually applies to `/events`, not just
    // the JSON routes exercised above.
    let unauthed = client.get(format!("{base}/events")).send().await.unwrap();
    assert_eq!(unauthed.status(), reqwest::StatusCode::UNAUTHORIZED);

    // ---- controller addition 2: unregistered approval id resolves false ----
    // `resolve_approval_route` must never 500/panic on an id nobody
    // registered (already resolved, unknown, or timed out) — it just reports
    // `resolved: false`.
    let resp = client
        .post(format!("{base}/approvals/nonexistent-run/nonexistent-id"))
        .bearer_auth(&token)
        .json(&json!({ "response": { "decision": "allowOnce", "scope": null, "payload": null } }))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), reqwest::StatusCode::OK);
    let body: serde_json::Value = resp.json().await.unwrap();
    assert_eq!(body, json!({"resolved": false}));

    daemon.stop().await;
}

/// ---- controller addition 3: endpoint autostart branch ----
///
/// With `endpoint_autostart` = "1" and `endpoint_port` = "0" (ephemeral)
/// persisted BEFORE `build_daemon` runs, `Daemon::start()` must bring the
/// local `RouterServer` up on its own — mirroring how `api/endpoint_api.rs`'s
/// `endpoint_status` reads `srv.status()` (see its `status_info` helper).
#[tokio::test]
async fn daemon_start_autostarts_the_endpoint_server_when_configured() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("ryuzi.sqlite");

    // Pre-seed settings via a throwaway `Store` handle, same pattern as
    // `daemon.rs`'s `build_daemon_wires_known_gateways_and_skips_unknown_ids`
    // test — the settings must exist BEFORE `build_daemon` opens its own
    // `Store` and `Daemon::start()` reads them. `endpoint_autostart`/
    // `endpoint_port` are not in the validated `SettingsStore` field catalog
    // (see `api/endpoint_api.rs`'s `set_endpoint_config`, which writes them
    // via `Store::set_setting` directly, not `SettingsStore::set`).
    {
        let store = Store::open(&db_path).await.unwrap();
        store
            .set_setting(WriteOrigin::User, "endpoint_autostart", "1")
            .await
            .unwrap();
        store
            .set_setting(WriteOrigin::User, "endpoint_port", "0")
            .await
            .unwrap();
    }

    let daemon = build_daemon(BuildDaemonOpts {
        db_path,
        config_root: tmp.path().to_path_buf(),
        telemetry: Some(Arc::new(NoopTelemetry)),
        extra_gateway_factories: vec![],
        harness_factory: None,
    })
    .await
    .unwrap();

    assert!(
        !daemon.router_server.status().running,
        "the endpoint server must not be running before start()"
    );

    daemon.start().await.unwrap();

    let status = daemon.router_server.status();
    assert!(
        status.running,
        "endpoint_autostart=1 must bring the endpoint server up during start()"
    );
    assert_ne!(
        status.port, 0,
        "an ephemeral (0) configured port must resolve to the real bound port"
    );

    daemon.stop().await;
    assert!(
        !daemon.router_server.status().running,
        "stop() must also tear the autostarted endpoint server back down"
    );
}
