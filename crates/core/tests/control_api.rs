//! End-to-end: a built daemon serving the control API — auth, rpc, SSE.

use ryuzi_core::daemon::{build_daemon, BuildDaemonOpts};
use ryuzi_core::serve::{serve, ApiState};
use ryuzi_core::store::Store;
use ryuzi_core::telemetry::NoopTelemetry;
use serde_json::json;
use std::sync::Arc;

#[tokio::test]
async fn daemon_control_api_serves_rpc_and_sse_end_to_end() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("ryuzi.sqlite");

    let daemon = build_daemon(BuildDaemonOpts {
        db_path,
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
            token: Some(token.clone()),
        },
        0,
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
        .post(format!("{base}/approvals/nonexistent-id"))
        .bearer_auth(&token)
        .json(&json!({"allow": true}))
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
        store.set_setting("endpoint_autostart", "1").await.unwrap();
        store.set_setting("endpoint_port", "0").await.unwrap();
    }

    let daemon = build_daemon(BuildDaemonOpts {
        db_path,
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
