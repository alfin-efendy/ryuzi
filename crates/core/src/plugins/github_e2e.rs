//! Task 13b, Step 4 — end-to-end proof of the REAL first-party GitHub connector
//! component (`plugins/github`) through the GENERIC WASM plugin pipeline.
//!
//! Everything here drives the actual compiled `github.wasm` (built once per test
//! process by [`crate::plugins::build_github_component_once`]) and the generic
//! seams shipped by Phases 1–5 — signature verification, the component-release
//! installer, `load_active_bundles`, the connector adapter, and the host OAuth
//! capability. There is deliberately NO github-specific host branch: `github`
//! is just signed data flowing through the same code path `mimo`/`opencode`
//! use.
//!
//! # What is (and isn't) exercised against the network
//! The component's tools target the hard-coded origin `https://api.github.com`
//! (see `plugins/github/src/logic.rs`), which is not overridable. Pointing that
//! at a local mock would require a DNS + TLS bypass inside the security-critical
//! [`AllowedHttpClient`](crate::plugins::capabilities::http) — a poor trade to
//! test logic that is already exhaustively unit-tested in the component crate.
//! So the connector e2e drives the real guest end-to-end across the host OAuth
//! boundary WITHOUT a live TLS hop (the `auth_status` "not connected" path and
//! the confirm-gate refusal path both round-trip guest→host→guest with no
//! network), and the "host injects the bearer / the component never sees the
//! token" guarantee is proven against a mock GitHub host at the exact host seam
//! the guest funnels into — [`ProfileOauth::authorized_request`] — using the
//! component's own declared `github` OAuth profile.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use ed25519_dalek::{Signer, SigningKey};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use ryuzi_plugin_sdk::{OAuthProfile, PluginBundleManifest};

use crate::domain::Principal;
use crate::plugins::build_github_component_once;
use crate::plugins::bundle::{load_active_bundles, ComponentBundleInstaller};
use crate::plugins::capabilities::oauth::{OauthErr, ProfileOauth};
use crate::plugins::capabilities::PluginCapabilityContext;
use crate::plugins::first_party_key::FIRST_PARTY_KEY_ID;
use crate::plugins::oauth::PluginOauthToken;
use crate::plugins::remote_catalog::{install_component_release, CatalogHttp};
use crate::plugins::runtime::{ComponentRuntime, HostPolicy};
use crate::plugins::wasm_connector::{wasm_tool_name, WasmActivation, WasmToolSet, WasmTools};
use crate::settings::SettingsStore;
use crate::store::{PluginOauthProfileClient, Store};
use crate::telemetry::NoopTelemetry;

// ---------------------------------------------------------------------------
// paths to the real, committed component + its freshly-built artifact
// ---------------------------------------------------------------------------

/// Repo-root-relative path from `crates/core` (this crate's manifest dir).
fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn github_manifest_path() -> PathBuf {
    repo_path("plugins/github/ryuzi-plugin.toml")
}

/// The github component's own manifest version — the single source of truth, so
/// these fixtures never mismatch it after a version bump. `install_verified`
/// requires the release descriptor's version to equal the manifest's.
fn github_manifest_version() -> String {
    let toml = std::fs::read_to_string(github_manifest_path()).unwrap();
    ryuzi_plugin_sdk::PluginBundleManifest::from_toml(&toml)
        .unwrap()
        .version
}

fn github_wasm_path() -> PathBuf {
    repo_path("plugins/github/target/wasm32-wasip2/release/ryuzi_plugin_github.wasm")
}

/// The component's own committed manifest — the single source of truth for the
/// tools' metadata, the network allowlist, and the declared `github` OAuth
/// profile that the OAuth e2e below drives.
fn github_manifest() -> PluginBundleManifest {
    let toml = std::fs::read_to_string(github_manifest_path())
        .expect("reading plugins/github/ryuzi-plugin.toml");
    PluginBundleManifest::from_toml(&toml).expect("parsing the github bundle manifest")
}

// ---------------------------------------------------------------------------
// signing helpers (a throwaway TEST key injected as the trusted first-party
// key — the real FIRST_PARTY_PUBKEY is the fail-closed all-zero placeholder)
// ---------------------------------------------------------------------------

fn test_key() -> SigningKey {
    SigningKey::from_bytes(&[42u8; 32])
}

fn trusted() -> HashMap<String, [u8; 32]> {
    HashMap::from([(
        FIRST_PARTY_KEY_ID.to_string(),
        test_key().verifying_key().to_bytes(),
    )])
}

fn b64url(bytes: &[u8]) -> String {
    use base64::Engine as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
}

/// The four signed release artifacts for the real github bundle, built the same
/// way `scripts/plugins/build-first-party.ts` does: the committed manifest
/// verbatim, the freshly-built wasm, a `PluginRelease` descriptor, and a
/// `plugin.sig` envelope signed over the exact release.json bytes.
struct GithubArtifacts {
    manifest_toml: Vec<u8>,
    release_json: Vec<u8>,
    sig_json: Vec<u8>,
    wasm: Vec<u8>,
    component_url: String,
}

fn build_github_artifacts(base: &str, key: &SigningKey, key_id: &str) -> GithubArtifacts {
    build_github_artifacts_with_wasm(
        base,
        key,
        key_id,
        std::fs::read(github_wasm_path()).unwrap(),
    )
}

/// Like [`build_github_artifacts`] but with caller-supplied wasm bytes — the
/// tamper test signs the release over the *real* wasm's hash but serves
/// different bytes.
fn build_github_artifacts_with_wasm(
    base: &str,
    key: &SigningKey,
    key_id: &str,
    wasm: Vec<u8>,
) -> GithubArtifacts {
    let manifest_toml = std::fs::read(github_manifest_path()).unwrap();
    let sha = format!("{:x}", Sha256::digest(&wasm));
    let component_url = format!("{base}/github.wasm");
    let release_json = serde_json::to_vec(&json!({
        "id": "github",
        "version": github_manifest_version(),
        "wit-api": "0.1.0",
        "component_url": component_url,
        "component_sha256": sha,
    }))
    .unwrap();
    let signature = key.sign(&release_json);
    let sig_json = serde_json::to_vec(&json!({
        "key_id": key_id,
        "signature": b64url(&signature.to_bytes()),
    }))
    .unwrap();
    GithubArtifacts {
        manifest_toml,
        release_json,
        sig_json,
        wasm,
        component_url,
    }
}

// ---------------------------------------------------------------------------
// a CatalogHttp fake serving canned bodies by exact URL (mirrors the mimo/
// opencode bootstrap tests' FakeReleaseHttp in remote_catalog.rs)
// ---------------------------------------------------------------------------

struct FakeReleaseHttp {
    routes: StdMutex<HashMap<String, (u16, Vec<u8>)>>,
}

impl FakeReleaseHttp {
    fn new() -> Self {
        Self {
            routes: StdMutex::new(HashMap::new()),
        }
    }

    fn put(&self, url: impl Into<String>, status: u16, bytes: Vec<u8>) {
        self.routes
            .lock()
            .unwrap()
            .insert(url.into(), (status, bytes));
    }

    /// Register the four artifacts of a latest (unpinned) github install at `base`.
    fn register(&self, base: &str, a: &GithubArtifacts) {
        self.put(
            format!("{base}/github.ryuzi-plugin.toml"),
            200,
            a.manifest_toml.clone(),
        );
        self.put(
            format!("{base}/github.release.json"),
            200,
            a.release_json.clone(),
        );
        self.put(
            format!("{base}/github.release.json.sig"),
            200,
            a.sig_json.clone(),
        );
        self.put(a.component_url.clone(), 200, a.wasm.clone());
    }
}

#[async_trait::async_trait]
impl CatalogHttp for FakeReleaseHttp {
    async fn get(&self, url: &str) -> anyhow::Result<(u16, Vec<u8>)> {
        Ok(self
            .routes
            .lock()
            .unwrap()
            .get(url)
            .cloned()
            .unwrap_or((404, vec![])))
    }
}

const BASE: &str = "http://feed.test/latest";

async fn test_store() -> (Arc<Store>, tempfile::NamedTempFile) {
    let tmp = tempfile::NamedTempFile::new().unwrap();
    let store = Arc::new(Store::open(tmp.path()).await.unwrap());
    (store, tmp)
}

/// Sign + install the real github bundle through the generic pipeline into a
/// throwaway root, returning the store + root so callers can load/compile it.
async fn install_real_github() -> (Arc<Store>, tempfile::NamedTempFile, tempfile::TempDir) {
    build_github_component_once();
    let (store, tmp) = test_store().await;
    let root = tempfile::tempdir().unwrap();
    let installer =
        ComponentBundleInstaller::new(root.path().to_path_buf(), store.as_ref().clone());
    let http = FakeReleaseHttp::new();
    http.register(
        BASE,
        &build_github_artifacts(BASE, &test_key(), FIRST_PARTY_KEY_ID),
    );
    install_component_release(&http, &installer, &trusted(), BASE, "github", None)
        .await
        .expect("the real github bundle must sign, verify, and install");
    (store, tmp, root)
}

// ===========================================================================
// Deliverable 1 — sign + install e2e through the generic pipeline
// ===========================================================================

#[tokio::test]
async fn github_release_signs_installs_and_loads_through_the_generic_pipeline() {
    build_github_component_once();
    let (store, _tmp) = test_store().await;
    let root = tempfile::tempdir().unwrap();
    let installer =
        ComponentBundleInstaller::new(root.path().to_path_buf(), store.as_ref().clone());
    let http = FakeReleaseHttp::new();
    http.register(
        BASE,
        &build_github_artifacts(BASE, &test_key(), FIRST_PARTY_KEY_ID),
    );

    let record = install_component_release(&http, &installer, &trusted(), BASE, "github", None)
        .await
        .expect("install must succeed for a correctly signed github release");

    // The release verified and activated.
    let version = github_manifest_version();
    assert_eq!(record.plugin_id, "github");
    assert_eq!(record.version, version);
    assert!(record.active);
    assert_eq!(record.signing_key_id, FIRST_PARTY_KEY_ID);

    // Installed to <root>/github/<version> with the active pointer set.
    assert!(root
        .path()
        .join(format!("github/{version}/github.wasm"))
        .is_file());
    assert_eq!(
        std::fs::read_to_string(root.path().join("github/current")).unwrap(),
        version
    );

    // The ledger row is active.
    assert_eq!(
        store
            .active_component_release("github")
            .await
            .unwrap()
            .unwrap()
            .version,
        version
    );

    // load_active_bundles surfaces it, with the real manifest (including the
    // declared `github` OAuth profile).
    let bundles = load_active_bundles(root.path(), &store).await.unwrap();
    let gh = bundles
        .iter()
        .find(|b| b.manifest.id == "github")
        .expect("the installed github bundle must be discovered");
    assert_eq!(gh.release.version, version);
    assert_eq!(gh.manifest.oauth.len(), 1);
    assert_eq!(gh.manifest.oauth[0].id, "github");
}

#[tokio::test]
async fn github_install_rejects_a_tampered_component() {
    build_github_component_once();
    let (store, _tmp) = test_store().await;
    let root = tempfile::tempdir().unwrap();
    let installer =
        ComponentBundleInstaller::new(root.path().to_path_buf(), store.as_ref().clone());
    let http = FakeReleaseHttp::new();
    // Sign the release over the REAL wasm's hash, then serve DIFFERENT wasm bytes.
    let a = build_github_artifacts(BASE, &test_key(), FIRST_PARTY_KEY_ID);
    http.register(BASE, &a);
    http.put(
        a.component_url.clone(),
        200,
        b"tampered github wasm".to_vec(),
    );

    let err = install_component_release(&http, &installer, &trusted(), BASE, "github", None)
        .await
        .expect_err("a tampered component must fail the hash check");
    assert!(
        format!("{err:#}").contains("hash mismatch"),
        "unexpected error: {err:#}"
    );
    assert!(store
        .active_component_release("github")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn github_install_rejects_an_untrusted_signing_key() {
    build_github_component_once();
    let (store, _tmp) = test_store().await;
    let root = tempfile::tempdir().unwrap();
    let installer =
        ComponentBundleInstaller::new(root.path().to_path_buf(), store.as_ref().clone());
    let http = FakeReleaseHttp::new();
    // Signed by a rogue key whose id is not in the trusted map.
    let rogue = SigningKey::from_bytes(&[7u8; 32]);
    http.register(BASE, &build_github_artifacts(BASE, &rogue, "rogue"));

    let err = install_component_release(&http, &installer, &trusted(), BASE, "github", None)
        .await
        .expect_err("an untrusted signing key must be rejected");
    assert!(
        format!("{err:#}").contains("untrusted signing key"),
        "unexpected error: {err:#}"
    );
    assert!(store
        .active_component_release("github")
        .await
        .unwrap()
        .is_none());
}

// ===========================================================================
// Deliverable 2 — connector e2e through the generic WasmConnector adapter
// ===========================================================================

/// Build a `WasmActivation` over the freshly-installed github bundle, using the
/// generic per-bundle host policy + capability context. The context is seeded
/// straight from the bundle's own manifest (network allowlist + declared OAuth
/// profile ids), exactly as the daemon does.
async fn github_activation(store: &Arc<Store>, root: &Path) -> Arc<WasmActivation> {
    let bundle = load_active_bundles(root, store)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.manifest.id == "github")
        .expect("installed github bundle");
    let policy = HostPolicy::for_installed_bundle(&bundle);
    let runtime = ComponentRuntime::new().unwrap();
    let compiled = Arc::new(
        runtime
            .compile(&bundle, policy)
            .expect("the real github component must compile with oauth linked"),
    );
    let ctx = Arc::new(PluginCapabilityContext {
        plugin_id: "github".to_string(),
        version: "0.1.0".to_string(),
        settings: SettingsStore::new(store.clone()),
        store: store.clone(),
        telemetry: Arc::new(NoopTelemetry),
        network_allowlist: bundle
            .manifest
            .permissions
            .network
            .iter()
            .map(|n| n.0.clone())
            .collect(),
        oauth_profile_ids: bundle.manifest.oauth.iter().map(|o| o.id.clone()).collect(),
        provider_ids: bundle.manifest.resolved_provider_ids(),
    });
    Arc::new(WasmActivation::new(
        compiled,
        ctx,
        "github".to_string(),
        Principal {
            plugin_id: "github".to_string(),
            plugin_name: "GitHub".to_string(),
        },
    ))
}

/// The exact `wasm__github__*` wire names for the `0.1.0` tool set.
fn expected_tool_names() -> Vec<String> {
    let mut names: Vec<String> = [
        "auth_status",
        "repo_get",
        "repo_list",
        "issue_list",
        "pr_list",
        "rest_get",
        "graphql",
        "issue_create",
        "issue_comment",
        "pr_create",
        "pr_review",
        "pr_merge",
    ]
    .iter()
    .copied()
    .map(|t| wasm_tool_name("github", t))
    .collect();
    names.sort();
    names
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_github_enumerates_the_0_1_0_connector_tools() {
    let (store, _tmp, root) = install_real_github().await;
    let activation = github_activation(&store, root.path()).await;
    let set = WasmToolSet::new(vec![activation]);

    let mut names: Vec<String> = set
        .session_tools()
        .await
        .into_iter()
        .map(|b| wasm_tool_name(&b.component_id, &b.def.name))
        .collect();
    names.sort();
    assert_eq!(names, expected_tool_names());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auth_status_without_a_token_reports_not_connected_end_to_end() {
    let (store, _tmp, root) = install_real_github().await;
    let activation = github_activation(&store, root.path()).await;

    // No token provisioned for (github, github): the guest's oauth call gets
    // `denied` from the host and maps it to the "not connected" probe result —
    // a full guest→host-oauth-import→guest round-trip with no network hop.
    let output = activation
        .connector_invoke("auth_status", json!({}))
        .await
        .expect("auth_status is a probe and must not error when disconnected");
    let text = output
        .as_str()
        .expect("auth_status returns a JSON text value");
    let parsed: Value = serde_json::from_str(text).unwrap();
    assert_eq!(parsed["connected"], false);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mutating_tool_without_confirm_is_refused_by_the_real_component() {
    let (store, _tmp, root) = install_real_github().await;
    let activation = github_activation(&store, root.path()).await;

    // The confirm gate lives inside the compiled component: pr_merge without
    // confirm=true must be refused BEFORE any request is planned or sent.
    let err = activation
        .connector_invoke(
            "pr_merge",
            json!({ "owner": "o", "repo": "r", "pr_number": 1 }),
        )
        .await
        .expect_err("an unconfirmed mutation must be refused by the component");
    let message = err.to_string();
    assert!(
        message.contains("confirm=true") || message.contains("mutating"),
        "unexpected error: {message}"
    );
}

// ===========================================================================
// Deliverable 3 — OAuth e2e driven by the component's declared `github` profile
// ===========================================================================

fn github_profile() -> OAuthProfile {
    github_manifest().oauth.into_iter().next().unwrap()
}

/// A capability context for the `github` plugin over a fresh store. `allowlist`
/// is the component's declared egress allowlist; the OAuth device/token flow is
/// host-driven and must NOT be bound by it (deliverable 5).
fn github_ctx(store: Arc<Store>, allowlist: Vec<&str>) -> PluginCapabilityContext {
    PluginCapabilityContext {
        plugin_id: "github".to_string(),
        version: "0.1.0".to_string(),
        settings: SettingsStore::new(store.clone()),
        store,
        telemetry: Arc::new(NoopTelemetry),
        network_allowlist: allowlist.into_iter().map(String::from).collect(),
        oauth_profile_ids: vec!["github".to_string()],
        provider_ids: vec![],
    }
}

/// Seed the out-of-band client id GitHub OAuth apps require (the manifest sets
/// `dynamic-registration = false` and no `client_id_setting`, so the host reads
/// it from the cached profile-client row).
async fn seed_github_client(store: &Store) {
    store
        .upsert_plugin_oauth_profile_client(&PluginOauthProfileClient {
            plugin_id: "github".to_string(),
            profile_id: "github".to_string(),
            authorize_url: None,
            token_url: None,
            client_id: Some("gh-client-123".to_string()),
        })
        .await
        .unwrap();
}

async fn spawn_server(app: axum::Router) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

#[tokio::test]
async fn github_device_flow_initiation_uses_the_declared_profile() {
    use axum::{routing::post, Form, Json};

    // A mock GitHub device-authorization endpoint asserting the request the host
    // builds from the manifest profile (client id + space-joined scopes).
    let app = axum::Router::new().route(
        "/login/device/code",
        post(|Form(form): Form<HashMap<String, String>>| async move {
            assert_eq!(
                form.get("client_id").map(String::as_str),
                Some("gh-client-123")
            );
            assert_eq!(
                form.get("scope").map(String::as_str),
                Some("repo read:org user"),
                "scopes must come from the github manifest profile"
            );
            Json(json!({
                "device_code": "3584d83530557fdd1f46af8289938c8ef79f9dc5",
                "user_code": "WDJB-MJHT",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900,
                "interval": 5,
            }))
        }),
    );
    let port = spawn_server(app).await;

    let (store, _tmp) = test_store().await;
    seed_github_client(&store).await;
    let ctx = github_ctx(store, vec!["api.github.com", "github.com"]);

    let start = ProfileOauth::new(&ctx)
        .begin_device_flow(
            &github_profile(),
            &format!("http://127.0.0.1:{port}/login/device/code"),
        )
        .await
        .expect("device flow initiation must succeed");

    assert_eq!(start.user_code, "WDJB-MJHT");
    assert_eq!(start.verification_uri, "https://github.com/login/device");
    assert_eq!(start.interval_secs, 5);
    assert_eq!(
        start.device_code,
        "3584d83530557fdd1f46af8289938c8ef79f9dc5"
    );
}

#[tokio::test]
async fn authorized_request_injects_the_bearer_and_never_leaks_the_token() {
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::get;

    // A mock api.github.com /user that records the Authorization it saw and
    // returns a normal user body (no token anywhere in it).
    let seen: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
    async fn user(
        State(seen): State<Arc<StdMutex<Option<String>>>>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        *seen.lock().unwrap() = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        r#"{"login":"octocat","name":"The Octocat"}"#
    }
    let app = axum::Router::new()
        .route("/user", get(user))
        .with_state(seen.clone());
    let port = spawn_server(app).await;

    let (store, _tmp) = test_store().await;
    store
        .upsert_plugin_oauth_profile_token(
            "github",
            "github",
            &PluginOauthToken {
                plugin_id: "github".to_string(),
                access_token: "real-github-token".to_string(),
                refresh_token: Some("real-github-refresh".to_string()),
                token_type: "Bearer".to_string(),
                expires_at: Some(crate::paths::now_ms() + 3_600_000),
                scopes: vec![],
                reconnect_required: false,
            },
        )
        .await
        .unwrap();
    // 127.0.0.1 is allowlisted here so the mock stands in for api.github.com.
    let ctx = github_ctx(store, vec!["127.0.0.1"]);

    // The component supplies a FORGED Authorization; the host must override it
    // with the stored bearer and the component must never receive either token.
    let response = ProfileOauth::new(&ctx)
        .authorized_request(
            "github",
            "GET",
            &format!("http://127.0.0.1:{port}/user"),
            vec![("authorization".to_string(), "Bearer forged".to_string())],
            None,
        )
        .await
        .expect("authorized_request must reach the mock host");

    // The mock saw the host's bearer, not the component's forged one.
    assert_eq!(
        *seen.lock().unwrap(),
        Some("Bearer real-github-token".to_string())
    );
    // The response handed back to the component carries neither the access nor
    // refresh token (in body or headers).
    let body = String::from_utf8_lossy(&response.body);
    assert!(body.contains("octocat"));
    assert!(!body.contains("real-github-token"));
    assert!(!body.contains("real-github-refresh"));
    assert!(!body.contains("forged"));
    assert!(!response
        .headers
        .iter()
        .any(|(_, v)| v.contains("real-github-token") || v.contains("real-github-refresh")));
}

// ===========================================================================
// Deliverable 5 — is the OAuth device/token flow bound by the component's
// network allowlist? Executable answer: NO for the host-driven device flow,
// YES for the component-driven authorized_request egress.
// ===========================================================================

#[tokio::test]
async fn device_flow_is_not_bound_by_the_component_network_allowlist() {
    use axum::{routing::post, Json};

    let app = axum::Router::new().route(
        "/login/device/code",
        post(|| async {
            Json(json!({
                "device_code": "dc",
                "user_code": "WDJB-MJHT",
                "verification_uri": "https://github.com/login/device",
                "expires_in": 900,
            }))
        }),
    );
    let port = spawn_server(app).await;

    let (store, _tmp) = test_store().await;
    seed_github_client(&store).await;
    // The allowlist deliberately EXCLUDES 127.0.0.1 (only the real github hosts).
    let ctx = github_ctx(store, vec!["api.github.com", "github.com"]);

    // begin_device_flow still reaches the 127.0.0.1 mock: it uses a plain
    // bounded client, NOT AllowedHttpClient(network_allowlist).
    let start = ProfileOauth::new(&ctx)
        .begin_device_flow(
            &github_profile(),
            &format!("http://127.0.0.1:{port}/login/device/code"),
        )
        .await
        .expect("the host-driven device flow must bypass the component allowlist");
    assert_eq!(start.user_code, "WDJB-MJHT");
}

#[tokio::test]
async fn authorized_request_is_bound_by_the_component_network_allowlist() {
    let (store, _tmp) = test_store().await;
    store
        .upsert_plugin_oauth_profile_token(
            "github",
            "github",
            &PluginOauthToken {
                plugin_id: "github".to_string(),
                access_token: "real-github-token".to_string(),
                refresh_token: None,
                token_type: "Bearer".to_string(),
                expires_at: Some(crate::paths::now_ms() + 3_600_000),
                scopes: vec![],
                reconnect_required: false,
            },
        )
        .await
        .unwrap();
    // Allowlist excludes 127.0.0.1: the component's own egress IS bound by it,
    // so a request to a non-allowlisted host is rejected (contrast the device
    // flow above). Token is present, so this fails at the allowlist, not the
    // token lookup.
    let ctx = github_ctx(store, vec!["api.github.com", "github.com"]);
    let err = ProfileOauth::new(&ctx)
        .authorized_request("github", "GET", "http://127.0.0.1:1/user", vec![], None)
        .await
        .expect_err("a non-allowlisted host must be rejected");
    match err {
        OauthErr::Failed(message) => assert!(
            message.contains("Rejected"),
            "expected an allowlist rejection, got: {message}"
        ),
        other => panic!("expected OauthErr::Failed(Rejected), got {other:?}"),
    }
}
