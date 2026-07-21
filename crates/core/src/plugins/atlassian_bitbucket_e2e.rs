//! Task 15c — end-to-end OAuth-profile ISOLATION proof for the REAL
//! first-party Atlassian (`plugins/atlassian`) and Bitbucket
//! (`plugins/bitbucket`) connector components, closing out Task 15.
//!
//! Mirrors [`crate::plugins::github_e2e`] (Task 13b): everything here drives
//! the actual compiled `atlassian.wasm`/`bitbucket.wasm` (built once per test
//! process by [`crate::plugins::build_atlassian_component_once`] /
//! [`crate::plugins::build_bitbucket_component_once`]) through the generic
//! seams shipped by Phases 1-5 — signature verification, the
//! component-release installer, `load_active_bundles`, the connector
//! adapter, and the host OAuth capability. There is deliberately NO
//! atlassian/bitbucket-specific host branch: both are just signed data
//! flowing through the same code path `github`/`mimo`/`opencode` use.
//!
//! # The isolation deliverable
//! Atlassian and Bitbucket are two SEPARATE signed bundles, each declaring
//! exactly one `[[oauth]]` profile in its own manifest — `atlassian-cloud`
//! and `bitbucket-cloud` respectively (see each `ryuzi-plugin.toml`). The
//! host derives each bundle's `PluginCapabilityContext.oauth_profile_ids`
//! straight from its own manifest, and every OAuth call is gated by
//! [`ProfileOauth::ensure_declared_profile`] plus a
//! `(plugin_id, profile_id)`-keyed token store
//! (`Store::get_plugin_oauth_profile_token`) — so isolation is a property of
//! the generic plumbing, not a special case. This file proves it end to end:
//! seeding a token ONLY for `(atlassian, atlassian-cloud)` lets BOTH a
//! Jira-style and a Confluence-style request through that ONE shared
//! profile, while every Bitbucket request is denied absent its own separate
//! `(bitbucket, bitbucket-cloud)` token, and each connector is refused if it
//! ever tries the other's profile id (an undeclared profile for it).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use sha2::{Digest, Sha256};

use ryuzi_plugin_sdk::PluginBundleManifest;

use crate::api::types::ComponentManifestInfo;
use crate::domain::Principal;
use crate::plugins::bundle::{load_active_bundles, ComponentBundleInstaller, InstalledBundle};
use crate::plugins::capabilities::oauth::{OauthErr, ProfileOauth};
use crate::plugins::capabilities::PluginCapabilityContext;
use crate::plugins::first_party_key::FIRST_PARTY_KEY_ID;
use crate::plugins::oauth::PluginOauthToken;
use crate::plugins::remote_catalog::{install_component_release, CatalogHttp};
use crate::plugins::runtime::{ComponentRuntime, HostPolicy};
use crate::plugins::wasm_connector::{wasm_tool_name, WasmActivation, WasmToolSet, WasmTools};
use crate::plugins::{build_atlassian_component_once, build_bitbucket_component_once};
use crate::settings::SettingsStore;
use crate::store::Store;
use crate::telemetry::NoopTelemetry;

// ---------------------------------------------------------------------------
// paths to the real, committed components + their freshly-built artifacts
// ---------------------------------------------------------------------------

/// Repo-root-relative path from `crates/core` (this crate's manifest dir).
fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn manifest_path(plugin_id: &str) -> PathBuf {
    repo_path(&format!("plugins/{plugin_id}/ryuzi-plugin.toml"))
}

fn wasm_path(plugin_id: &str, crate_wasm_stem: &str) -> PathBuf {
    repo_path(&format!(
        "plugins/{plugin_id}/target/wasm32-wasip2/release/{crate_wasm_stem}.wasm"
    ))
}

fn atlassian_manifest_path() -> PathBuf {
    manifest_path("atlassian")
}
fn atlassian_wasm_path() -> PathBuf {
    wasm_path("atlassian", "ryuzi_plugin_atlassian")
}
fn bitbucket_manifest_path() -> PathBuf {
    manifest_path("bitbucket")
}
fn bitbucket_wasm_path() -> PathBuf {
    wasm_path("bitbucket", "ryuzi_plugin_bitbucket")
}

/// Each component's own committed manifest — the single source of truth for
/// its declared OAuth profile(s) and network allowlist.
fn atlassian_manifest() -> PluginBundleManifest {
    let toml = std::fs::read_to_string(atlassian_manifest_path())
        .expect("reading plugins/atlassian/ryuzi-plugin.toml");
    PluginBundleManifest::from_toml(&toml).expect("parsing the atlassian bundle manifest")
}

fn bitbucket_manifest() -> PluginBundleManifest {
    let toml = std::fs::read_to_string(bitbucket_manifest_path())
        .expect("reading plugins/bitbucket/ryuzi-plugin.toml");
    PluginBundleManifest::from_toml(&toml).expect("parsing the bitbucket bundle manifest")
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

/// The four signed release artifacts for one real bundle, built the same way
/// `scripts/plugins/build-first-party.ts` does: the committed manifest
/// verbatim, the freshly-built wasm, a `PluginRelease` descriptor, and a
/// `plugin.sig` envelope signed over the exact release.json bytes.
struct ComponentArtifacts {
    manifest_toml: Vec<u8>,
    release_json: Vec<u8>,
    sig_json: Vec<u8>,
    wasm: Vec<u8>,
    component_url: String,
}

fn build_artifacts(
    plugin_id: &str,
    manifest_path: &Path,
    wasm_path: &Path,
    base: &str,
    key: &SigningKey,
    key_id: &str,
) -> ComponentArtifacts {
    let manifest_toml = std::fs::read(manifest_path).unwrap();
    let wasm = std::fs::read(wasm_path).unwrap();
    let sha = format!("{:x}", Sha256::digest(&wasm));
    let component_url = format!("{base}/{plugin_id}.wasm");
    let release_json = serde_json::to_vec(&json!({
        "id": plugin_id,
        "version": "0.1.0",
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
    ComponentArtifacts {
        manifest_toml,
        release_json,
        sig_json,
        wasm,
        component_url,
    }
}

// ---------------------------------------------------------------------------
// a CatalogHttp fake serving canned bodies by exact URL (mirrors github_e2e's
// / discord_e2e's FakeReleaseHttp) — one instance serves BOTH plugins' four
// artifacts, since every URL is qualified by plugin id and never collides.
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

    /// Register the four artifacts of a latest (unpinned) install for
    /// `plugin_id` at `base`.
    fn register(&self, plugin_id: &str, base: &str, a: &ComponentArtifacts) {
        self.put(
            format!("{base}/{plugin_id}.ryuzi-plugin.toml"),
            200,
            a.manifest_toml.clone(),
        );
        self.put(
            format!("{base}/{plugin_id}.release.json"),
            200,
            a.release_json.clone(),
        );
        self.put(
            format!("{base}/{plugin_id}.release.json.sig"),
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

/// Sign + install BOTH real bundles through the generic pipeline into a
/// shared throwaway root, returning the store + root so callers can
/// load/compile them.
async fn install_atlassian_and_bitbucket(
) -> (Arc<Store>, tempfile::NamedTempFile, tempfile::TempDir) {
    build_atlassian_component_once();
    build_bitbucket_component_once();
    let (store, tmp) = test_store().await;
    let root = tempfile::tempdir().unwrap();
    let installer =
        ComponentBundleInstaller::new(root.path().to_path_buf(), store.as_ref().clone());
    let http = FakeReleaseHttp::new();
    http.register(
        "atlassian",
        BASE,
        &build_artifacts(
            "atlassian",
            &atlassian_manifest_path(),
            &atlassian_wasm_path(),
            BASE,
            &test_key(),
            FIRST_PARTY_KEY_ID,
        ),
    );
    http.register(
        "bitbucket",
        BASE,
        &build_artifacts(
            "bitbucket",
            &bitbucket_manifest_path(),
            &bitbucket_wasm_path(),
            BASE,
            &test_key(),
            FIRST_PARTY_KEY_ID,
        ),
    );
    install_component_release(&http, &installer, &trusted(), BASE, "atlassian", None)
        .await
        .expect("the real atlassian bundle must sign, verify, and install");
    install_component_release(&http, &installer, &trusted(), BASE, "bitbucket", None)
        .await
        .expect("the real bitbucket bundle must sign, verify, and install");
    (store, tmp, root)
}

async fn spawn_server(app: axum::Router) -> u16 {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    port
}

// ===========================================================================
// Deliverable 1 — sign + install e2e through the generic pipeline, for BOTH
// bundles, each surfacing its own single declared OAuth profile
// ===========================================================================

#[tokio::test]
async fn atlassian_and_bitbucket_releases_sign_install_and_load_through_the_generic_pipeline() {
    let (store, _tmp, root) = install_atlassian_and_bitbucket().await;

    assert_eq!(
        store
            .active_component_release("atlassian")
            .await
            .unwrap()
            .unwrap()
            .version,
        "0.1.0"
    );
    assert_eq!(
        store
            .active_component_release("bitbucket")
            .await
            .unwrap()
            .unwrap()
            .version,
        "0.1.0"
    );

    let bundles = load_active_bundles(root.path(), &store).await.unwrap();
    let atlassian = bundles
        .iter()
        .find(|b| b.manifest.id == "atlassian")
        .expect("the installed atlassian bundle must be discovered");
    let bitbucket = bundles
        .iter()
        .find(|b| b.manifest.id == "bitbucket")
        .expect("the installed bitbucket bundle must be discovered");

    // Each bundle declares EXACTLY one OAuth profile, and the two ids never
    // collide — the isolation proof's precondition.
    assert_eq!(atlassian.manifest.oauth.len(), 1);
    assert_eq!(atlassian.manifest.oauth[0].id, "atlassian-cloud");
    assert_eq!(bitbucket.manifest.oauth.len(), 1);
    assert_eq!(bitbucket.manifest.oauth[0].id, "bitbucket-cloud");
    assert_ne!(
        atlassian.manifest.oauth[0].id,
        bitbucket.manifest.oauth[0].id
    );
}

#[tokio::test]
async fn atlassian_install_rejects_a_tampered_component() {
    build_atlassian_component_once();
    let (store, _tmp) = test_store().await;
    let root = tempfile::tempdir().unwrap();
    let installer =
        ComponentBundleInstaller::new(root.path().to_path_buf(), store.as_ref().clone());
    let http = FakeReleaseHttp::new();
    let a = build_artifacts(
        "atlassian",
        &atlassian_manifest_path(),
        &atlassian_wasm_path(),
        BASE,
        &test_key(),
        FIRST_PARTY_KEY_ID,
    );
    http.register("atlassian", BASE, &a);
    http.put(
        a.component_url.clone(),
        200,
        b"tampered atlassian wasm".to_vec(),
    );

    let err = install_component_release(&http, &installer, &trusted(), BASE, "atlassian", None)
        .await
        .expect_err("a tampered component must fail the hash check");
    assert!(
        format!("{err:#}").contains("hash mismatch"),
        "unexpected error: {err:#}"
    );
    assert!(store
        .active_component_release("atlassian")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn bitbucket_install_rejects_an_untrusted_signing_key() {
    build_bitbucket_component_once();
    let (store, _tmp) = test_store().await;
    let root = tempfile::tempdir().unwrap();
    let installer =
        ComponentBundleInstaller::new(root.path().to_path_buf(), store.as_ref().clone());
    let http = FakeReleaseHttp::new();
    // Signed by a rogue key whose id is not in the trusted map.
    let rogue = SigningKey::from_bytes(&[7u8; 32]);
    http.register(
        "bitbucket",
        BASE,
        &build_artifacts(
            "bitbucket",
            &bitbucket_manifest_path(),
            &bitbucket_wasm_path(),
            BASE,
            &rogue,
            "rogue",
        ),
    );

    let err = install_component_release(&http, &installer, &trusted(), BASE, "bitbucket", None)
        .await
        .expect_err("an untrusted signing key must be rejected");
    assert!(
        format!("{err:#}").contains("untrusted signing key"),
        "unexpected error: {err:#}"
    );
    assert!(store
        .active_component_release("bitbucket")
        .await
        .unwrap()
        .is_none());
}

// ===========================================================================
// Deliverable 2 — connector e2e through the generic WasmConnector adapter
// (also proves both components compile + instantiate — the discord_e2e-style
// "confirm they load" check, reused here rather than duplicated separately)
// ===========================================================================

async fn activation_for(bundle: &InstalledBundle, store: &Arc<Store>) -> Arc<WasmActivation> {
    let policy = HostPolicy::for_installed_bundle(bundle);
    let runtime = ComponentRuntime::new().unwrap();
    let compiled =
        Arc::new(runtime.compile(bundle, policy).unwrap_or_else(|e| {
            panic!("{} must compile with oauth linked: {e}", bundle.manifest.id)
        }));
    let ctx = Arc::new(PluginCapabilityContext {
        plugin_id: bundle.manifest.id.clone(),
        version: bundle.release.version.clone(),
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
    });
    Arc::new(WasmActivation::new(
        compiled,
        ctx,
        bundle.manifest.id.clone(),
        Principal {
            plugin_id: bundle.manifest.id.clone(),
            plugin_name: bundle.manifest.id.clone(),
        },
    ))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_atlassian_enumerates_jira_and_confluence_tools() {
    let (store, _tmp, root) = install_atlassian_and_bitbucket().await;
    let bundle = load_active_bundles(root.path(), &store)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.manifest.id == "atlassian")
        .unwrap();
    let activation = activation_for(&bundle, &store).await;
    let set = WasmToolSet::new(vec![activation]);

    let mut names: Vec<String> = set
        .session_tools()
        .await
        .into_iter()
        .map(|b| wasm_tool_name(&b.component_id, &b.def.name))
        .collect();
    names.sort();

    let mut expected: Vec<String> = [
        "auth_status",
        "jira_search",
        "jira_issue_get",
        "jira_issue_create",
        "jira_issue_comment",
        "jira_issue_transition",
        "confluence_search",
        "confluence_page_get",
        "confluence_page_create",
        "confluence_page_update",
    ]
    .iter()
    .copied()
    .map(|t| wasm_tool_name("atlassian", t))
    .collect();
    expected.sort();
    assert_eq!(names, expected);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn installed_bitbucket_enumerates_its_0_1_0_connector_tools() {
    let (store, _tmp, root) = install_atlassian_and_bitbucket().await;
    let bundle = load_active_bundles(root.path(), &store)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.manifest.id == "bitbucket")
        .unwrap();
    let activation = activation_for(&bundle, &store).await;
    let set = WasmToolSet::new(vec![activation]);

    let mut names: Vec<String> = set
        .session_tools()
        .await
        .into_iter()
        .map(|b| wasm_tool_name(&b.component_id, &b.def.name))
        .collect();
    names.sort();

    let mut expected: Vec<String> = [
        "auth_status",
        "repo_list",
        "repo_get",
        "pr_list",
        "issue_list",
        "pr_create",
        "pr_merge",
        "issue_create",
        "pr_comment",
    ]
    .iter()
    .copied()
    .map(|t| wasm_tool_name("bitbucket", t))
    .collect();
    expected.sort();
    assert_eq!(names, expected);
}

// ===========================================================================
// Deliverable 3 — THE isolation proof: a token seeded ONLY for
// (atlassian, atlassian-cloud) serves both Jira- and Confluence-style
// requests through that one profile, while Bitbucket is denied absent its
// own separate token, and each connector is refused the other's profile id.
// ===========================================================================

/// A capability context sourced straight from a real installed bundle's own
/// manifest (mirrors `github_e2e::github_activation`'s ctx construction), with
/// a caller-supplied `network_allowlist` override so the isolation test can
/// point egress at a loopback mock — the same override `github_e2e`'s
/// `github_ctx` helper uses, since the OAuth-authorized egress is bound by
/// this allowlist (Deliverable 5 in `github_e2e`) and the real manifest hosts
/// (`api.atlassian.com`/`api.bitbucket.org`) are not reachable in a test.
fn ctx_for(
    bundle: &InstalledBundle,
    store: Arc<Store>,
    network_allowlist: Vec<&str>,
) -> PluginCapabilityContext {
    PluginCapabilityContext {
        plugin_id: bundle.manifest.id.clone(),
        version: bundle.release.version.clone(),
        settings: SettingsStore::new(store.clone()),
        store,
        telemetry: Arc::new(NoopTelemetry),
        network_allowlist: network_allowlist.into_iter().map(String::from).collect(),
        oauth_profile_ids: bundle.manifest.oauth.iter().map(|o| o.id.clone()).collect(),
    }
}

#[tokio::test]
async fn one_atlassian_cloud_token_serves_jira_and_confluence_but_bitbucket_needs_its_own() {
    use axum::extract::State;
    use axum::http::HeaderMap;
    use axum::response::IntoResponse;
    use axum::routing::get;

    // A mock Atlassian API gateway with two routes shaped like the real
    // `api.atlassian.com/ex/{jira,confluence}/{cloudid}/...` gateway the
    // manifest documents — recording the Authorization each one saw.
    let seen_jira: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));
    let seen_confluence: Arc<StdMutex<Option<String>>> = Arc::new(StdMutex::new(None));

    async fn jira_search(
        State(seen): State<Arc<StdMutex<Option<String>>>>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        *seen.lock().unwrap() = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        r#"{"issues":[{"key":"PROJ-1"}]}"#
    }
    async fn confluence_content(
        State(seen): State<Arc<StdMutex<Option<String>>>>,
        headers: HeaderMap,
    ) -> impl IntoResponse {
        *seen.lock().unwrap() = headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .map(String::from);
        r#"{"results":[{"id":"999"}]}"#
    }
    let app = axum::Router::new()
        .route(
            "/ex/jira/cloud-1/rest/api/3/search",
            get(jira_search).with_state(seen_jira.clone()),
        )
        .route(
            "/ex/confluence/cloud-1/wiki/rest/api/content",
            get(confluence_content).with_state(seen_confluence.clone()),
        );
    let port = spawn_server(app).await;

    let (store, _tmp, root) = install_atlassian_and_bitbucket().await;
    let bundles = load_active_bundles(root.path(), &store).await.unwrap();
    let atlassian_bundle = bundles
        .iter()
        .find(|b| b.manifest.id == "atlassian")
        .unwrap();
    let bitbucket_bundle = bundles
        .iter()
        .find(|b| b.manifest.id == "bitbucket")
        .unwrap();

    // The atlassian bundle's context declares EXACTLY the atlassian-cloud
    // profile; the bitbucket bundle's declares EXACTLY bitbucket-cloud.
    let atlassian_ctx = ctx_for(atlassian_bundle, store.clone(), vec!["127.0.0.1"]);
    let bitbucket_ctx = ctx_for(bitbucket_bundle, store.clone(), vec!["127.0.0.1"]);
    assert_eq!(
        atlassian_ctx.oauth_profile_ids,
        vec!["atlassian-cloud".to_string()]
    );
    assert_eq!(
        bitbucket_ctx.oauth_profile_ids,
        vec!["bitbucket-cloud".to_string()]
    );

    // Seed a token ONLY for (atlassian, atlassian-cloud) — deliberately no
    // token anywhere for (bitbucket, bitbucket-cloud).
    store
        .upsert_plugin_oauth_profile_token(
            "atlassian",
            "atlassian-cloud",
            &PluginOauthToken {
                plugin_id: "atlassian".to_string(),
                access_token: "real-atlassian-token".to_string(),
                refresh_token: Some("real-atlassian-refresh".to_string()),
                token_type: "Bearer".to_string(),
                expires_at: Some(crate::paths::now_ms() + 3_600_000),
                scopes: vec![],
                reconnect_required: false,
            },
        )
        .await
        .unwrap();

    let atlassian_oauth = ProfileOauth::new(&atlassian_ctx);

    // A Jira-style request over the atlassian-cloud profile succeeds.
    let jira_response = atlassian_oauth
        .authorized_request(
            "atlassian-cloud",
            "GET",
            &format!("http://127.0.0.1:{port}/ex/jira/cloud-1/rest/api/3/search"),
            vec![],
            None,
        )
        .await
        .expect("a Jira-style request must succeed over the shared atlassian-cloud token");

    // A Confluence-style request over the SAME atlassian-cloud profile also
    // succeeds — one token serves both products.
    let confluence_response = atlassian_oauth
        .authorized_request(
            "atlassian-cloud",
            "GET",
            &format!("http://127.0.0.1:{port}/ex/confluence/cloud-1/wiki/rest/api/content"),
            vec![],
            None,
        )
        .await
        .expect(
            "a Confluence-style request must succeed over the SAME shared atlassian-cloud token",
        );

    assert!(String::from_utf8_lossy(&jira_response.body).contains("PROJ-1"));
    assert!(String::from_utf8_lossy(&confluence_response.body).contains("999"));
    assert_eq!(
        *seen_jira.lock().unwrap(),
        Some("Bearer real-atlassian-token".to_string())
    );
    assert_eq!(
        *seen_confluence.lock().unwrap(),
        Some("Bearer real-atlassian-token".to_string()),
        "Jira and Confluence must be authorized by the exact same atlassian-cloud bearer"
    );

    // Bitbucket, over its OWN declared bitbucket-cloud profile, is denied —
    // no token was ever seeded for (bitbucket, bitbucket-cloud), proving the
    // atlassian-cloud token does NOT serve it.
    let bitbucket_denied = ProfileOauth::new(&bitbucket_ctx)
        .authorized_request(
            "bitbucket-cloud",
            "GET",
            &format!("http://127.0.0.1:{port}/2.0/user"),
            vec![],
            None,
        )
        .await
        .expect_err("bitbucket must be denied without its own separate token");
    assert_eq!(bitbucket_denied, OauthErr::Denied);

    // Cross-plugin: the atlassian ctx trying the bitbucket profile id is an
    // UNDECLARED profile for it (ensure_declared_profile), not merely a
    // missing token — proves the profiles are isolated per plugin, not just
    // per missing-token happenstance.
    let atlassian_using_bitbucket_profile = ProfileOauth::new(&atlassian_ctx)
        .authorized_request(
            "bitbucket-cloud",
            "GET",
            "http://127.0.0.1:1/unreachable",
            vec![],
            None,
        )
        .await
        .expect_err("the atlassian context must refuse a profile id it never declared");
    assert_eq!(atlassian_using_bitbucket_profile, OauthErr::Denied);

    // And vice versa: bitbucket trying the atlassian-cloud profile id, even
    // though a real token exists for (atlassian, atlassian-cloud) — it must
    // never be reachable from the bitbucket plugin id.
    let bitbucket_using_atlassian_profile = ProfileOauth::new(&bitbucket_ctx)
        .authorized_request("atlassian-cloud", "GET", "http://127.0.0.1:1/unreachable", vec![], None)
        .await
        .expect_err("the bitbucket context must refuse a profile id it never declared, even though a real atlassian-cloud token exists");
    assert_eq!(bitbucket_using_atlassian_profile, OauthErr::Denied);
}

// ===========================================================================
// Deliverable 4 — Cockpit-independence check (a): the RPC-facing
// `ComponentManifestInfo` conversion (`crates/core/src/api/types.rs`) reports
// each connector's OWN, disjoint OAuth profile set — never a shared/merged
// one — the exact shape `plugin_release_detail` hands Cockpit's
// `PluginDetailView` (see `crates/core/src/api/plugins_api.rs`). This is a
// pure conversion over the committed manifests, so it needs no wasm build or
// install and runs instantly.
// ===========================================================================

#[test]
fn component_manifest_info_reports_separate_oauth_profiles_for_atlassian_and_bitbucket() {
    let atlassian_info = ComponentManifestInfo::from(atlassian_manifest());
    let bitbucket_info = ComponentManifestInfo::from(bitbucket_manifest());

    let atlassian_ids: Vec<&str> = atlassian_info
        .oauth_profiles
        .iter()
        .map(|p| p.id.as_str())
        .collect();
    let bitbucket_ids: Vec<&str> = bitbucket_info
        .oauth_profiles
        .iter()
        .map(|p| p.id.as_str())
        .collect();

    assert_eq!(atlassian_ids, vec!["atlassian-cloud"]);
    assert_eq!(bitbucket_ids, vec!["bitbucket-cloud"]);
    // Disjoint — the two RPC-facing profile sets never overlap, so Cockpit's
    // `PluginDetailView` can never render (or imply) a shared token.
    assert!(atlassian_ids.iter().all(|id| !bitbucket_ids.contains(id)));
    assert!(bitbucket_ids.iter().all(|id| !atlassian_ids.contains(id)));
}
