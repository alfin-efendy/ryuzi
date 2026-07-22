//! Task 10b — de-risking proof that the REAL first-party Discord gateway
//! component (`plugins/discord`) compiles, installs, and INSTANTIATES through
//! the host [`ComponentRuntime`] with the `ryuzi:websocket` capability linked.
//!
//! This is the gateway analog of the GitHub connector pilot
//! ([`crate::plugins::github_e2e`]): everything here drives the actual
//! compiled `discord.wasm` (built once per test process by
//! [`crate::plugins::build_discord_component_once`]) through the generic
//! seams shipped by Phases 1-5 — signature verification, the
//! component-release installer, `load_active_bundles`, and
//! `ComponentRuntime::compile` + `CompiledComponent::instantiate`. There is
//! deliberately NO discord-specific host branch: `discord` is just signed
//! data flowing through the same code path `mimo`/`opencode`/`github` use.
//!
//! Instantiation alone (never calling the guest's `start` export, which needs
//! a live network) is the proof: it exercises component compilation, import
//! resolution against the host policy, and linking all four capability
//! adapters (websocket/http/settings/storage) plus the `ryuzi:gateway` world
//! export validation — catching any import/world mismatch BEFORE the user's
//! expensive real-bot smoke test.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Mutex as StdMutex;

use ed25519_dalek::{Signer, SigningKey};
use serde_json::json;
use sha2::{Digest, Sha256};

use ryuzi_plugin_sdk::{PluginBundleManifest, PluginLifecycle};

use crate::plugins::build_discord_component_once;
use crate::plugins::bundle::{load_active_bundles, ComponentBundleInstaller};
use crate::plugins::capabilities::PluginCapabilityContext;
use crate::plugins::first_party_key::FIRST_PARTY_KEY_ID;
use crate::plugins::remote_catalog::{install_component_release, CatalogHttp};
use crate::plugins::runtime::{ComponentRuntime, HostPolicy};
use crate::settings::SettingsStore;
use crate::store::Store;
use crate::telemetry::NoopTelemetry;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// paths to the real, committed component + its freshly-built artifact
// ---------------------------------------------------------------------------

/// Repo-root-relative path from `crates/core` (this crate's manifest dir).
fn repo_path(rel: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
}

fn discord_manifest_path() -> PathBuf {
    repo_path("plugins/discord/ryuzi-plugin.toml")
}

fn discord_wasm_path() -> PathBuf {
    repo_path("plugins/discord/target/wasm32-wasip2/release/ryuzi_plugin_discord.wasm")
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

/// The four signed release artifacts for the real discord bundle, built the
/// same way `scripts/plugins/build-first-party.ts` does: the committed
/// manifest verbatim, the freshly-built wasm, a `PluginRelease` descriptor,
/// and a `plugin.sig` envelope signed over the exact release.json bytes.
struct DiscordArtifacts {
    manifest_toml: Vec<u8>,
    release_json: Vec<u8>,
    sig_json: Vec<u8>,
    wasm: Vec<u8>,
    component_url: String,
}

fn build_discord_artifacts(base: &str, key: &SigningKey, key_id: &str) -> DiscordArtifacts {
    build_discord_artifacts_with_wasm(
        base,
        key,
        key_id,
        std::fs::read(discord_wasm_path()).unwrap(),
    )
}

/// Like [`build_discord_artifacts`] but with caller-supplied wasm bytes — the
/// tamper test signs the release over the *real* wasm's hash but serves
/// different bytes.
fn build_discord_artifacts_with_wasm(
    base: &str,
    key: &SigningKey,
    key_id: &str,
    wasm: Vec<u8>,
) -> DiscordArtifacts {
    let manifest_toml = std::fs::read(discord_manifest_path()).unwrap();
    let sha = format!("{:x}", Sha256::digest(&wasm));
    let component_url = format!("{base}/discord.wasm");
    let release_json = serde_json::to_vec(&json!({
        "id": "discord",
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
    DiscordArtifacts {
        manifest_toml,
        release_json,
        sig_json,
        wasm,
        component_url,
    }
}

// ---------------------------------------------------------------------------
// a CatalogHttp fake serving canned bodies by exact URL (mirrors github_e2e's
// FakeReleaseHttp / the mimo/opencode bootstrap tests in remote_catalog.rs)
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

    /// Register the four artifacts of a latest (unpinned) discord install at `base`.
    fn register(&self, base: &str, a: &DiscordArtifacts) {
        self.put(
            format!("{base}/discord.ryuzi-plugin.toml"),
            200,
            a.manifest_toml.clone(),
        );
        self.put(
            format!("{base}/discord.release.json"),
            200,
            a.release_json.clone(),
        );
        self.put(
            format!("{base}/discord.release.json.sig"),
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

// ===========================================================================
// Deliverable 1 — sign + install e2e through the generic pipeline
// ===========================================================================

#[tokio::test]
async fn discord_release_signs_installs_and_loads_through_the_generic_pipeline() {
    build_discord_component_once();
    let (store, _tmp) = test_store().await;
    let root = tempfile::tempdir().unwrap();
    let installer =
        ComponentBundleInstaller::new(root.path().to_path_buf(), store.as_ref().clone());
    let http = FakeReleaseHttp::new();
    http.register(
        BASE,
        &build_discord_artifacts(BASE, &test_key(), FIRST_PARTY_KEY_ID),
    );

    let record = install_component_release(&http, &installer, &trusted(), BASE, "discord", None)
        .await
        .expect("install must succeed for a correctly signed discord release");

    // The release verified and activated.
    assert_eq!(record.plugin_id, "discord");
    assert_eq!(record.version, "0.1.0");
    assert!(record.active);
    assert_eq!(record.signing_key_id, FIRST_PARTY_KEY_ID);

    // Installed to <root>/discord/0.1.0 with the active pointer set.
    assert!(root.path().join("discord/0.1.0/discord.wasm").is_file());
    assert_eq!(
        std::fs::read_to_string(root.path().join("discord/current")).unwrap(),
        "0.1.0"
    );

    // The ledger row is active.
    assert_eq!(
        store
            .active_component_release("discord")
            .await
            .unwrap()
            .unwrap()
            .version,
        "0.1.0"
    );

    // load_active_bundles surfaces it, with the real manifest: singleton
    // lifecycle, the declared network allowlist (including the websocket
    // gateway hosts), and no OAuth profiles (a gateway component, not a
    // connector).
    let bundles = load_active_bundles(root.path(), &store).await.unwrap();
    let discord = bundles
        .iter()
        .find(|b| b.manifest.id == "discord")
        .expect("the installed discord bundle must be discovered");
    assert_eq!(discord.release.version, "0.1.0");
    assert_eq!(discord.manifest.lifecycle, PluginLifecycle::Singleton);
    assert!(discord.manifest.oauth.is_empty());
    let hosts: Vec<&str> = discord
        .manifest
        .permissions
        .network
        .iter()
        .map(|n| n.0.as_str())
        .collect();
    for expected in ["gateway.discord.gg", "*.discord.gg", "discord.com"] {
        assert!(
            hosts.contains(&expected),
            "expected {expected} in declared network hosts, got {hosts:?}"
        );
    }
}

#[tokio::test]
async fn discord_install_rejects_a_tampered_component() {
    build_discord_component_once();
    let (store, _tmp) = test_store().await;
    let root = tempfile::tempdir().unwrap();
    let installer =
        ComponentBundleInstaller::new(root.path().to_path_buf(), store.as_ref().clone());
    let http = FakeReleaseHttp::new();
    // Sign the release over the REAL wasm's hash, then serve DIFFERENT wasm bytes.
    let a = build_discord_artifacts(BASE, &test_key(), FIRST_PARTY_KEY_ID);
    http.register(BASE, &a);
    http.put(
        a.component_url.clone(),
        200,
        b"tampered discord wasm".to_vec(),
    );

    let err = install_component_release(&http, &installer, &trusted(), BASE, "discord", None)
        .await
        .expect_err("a tampered component must fail the hash check");
    assert!(
        format!("{err:#}").contains("hash mismatch"),
        "unexpected error: {err:#}"
    );
    assert!(store
        .active_component_release("discord")
        .await
        .unwrap()
        .is_none());
}

#[tokio::test]
async fn discord_install_rejects_an_untrusted_signing_key() {
    build_discord_component_once();
    let (store, _tmp) = test_store().await;
    let root = tempfile::tempdir().unwrap();
    let installer =
        ComponentBundleInstaller::new(root.path().to_path_buf(), store.as_ref().clone());
    let http = FakeReleaseHttp::new();
    // Signed by a rogue key whose id is not in the trusted map.
    let rogue = SigningKey::from_bytes(&[7u8; 32]);
    http.register(BASE, &build_discord_artifacts(BASE, &rogue, "rogue"));

    let err = install_component_release(&http, &installer, &trusted(), BASE, "discord", None)
        .await
        .expect_err("an untrusted signing key must be rejected");
    assert!(
        format!("{err:#}").contains("untrusted signing key"),
        "unexpected error: {err:#}"
    );
    assert!(store
        .active_component_release("discord")
        .await
        .unwrap()
        .is_none());
}

// ===========================================================================
// Deliverable 2 — the key assertion: the real discord.wasm INSTANTIATES
// through ComponentRuntime with the websocket capability linked
// ===========================================================================

/// Sanity check that the manifest actually still declares its own id/wit-api
/// the way the artifact builder above assumes (belt-and-suspenders — a drift
/// here would silently invalidate the signed release.json's metadata).
fn discord_manifest() -> PluginBundleManifest {
    let toml = std::fs::read_to_string(discord_manifest_path()).expect("reading discord manifest");
    PluginBundleManifest::from_toml(&toml).expect("parsing the discord bundle manifest")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_real_discord_component_instantiates_with_websocket_linked() {
    build_discord_component_once();
    let (store, _tmp) = test_store().await;
    let root = tempfile::tempdir().unwrap();
    let installer =
        ComponentBundleInstaller::new(root.path().to_path_buf(), store.as_ref().clone());
    let http = FakeReleaseHttp::new();
    http.register(
        BASE,
        &build_discord_artifacts(BASE, &test_key(), FIRST_PARTY_KEY_ID),
    );
    install_component_release(&http, &installer, &trusted(), BASE, "discord", None)
        .await
        .expect("the real discord bundle must sign, verify, and install");

    let bundle = load_active_bundles(root.path(), &store)
        .await
        .unwrap()
        .into_iter()
        .find(|b| b.manifest.id == "discord")
        .expect("installed discord bundle");

    // The manifest declares a non-empty network allowlist, so the derived
    // policy must grant both network and websocket.
    let policy = HostPolicy::for_installed_bundle(&bundle);
    assert!(
        policy.allow_network,
        "the discord manifest's network hosts must grant allow_network"
    );
    assert!(
        policy.allow_websocket,
        "the discord manifest's network hosts must grant allow_websocket"
    );

    let runtime = ComponentRuntime::new().expect("runtime should configure");
    let compiled = runtime
        .compile(&bundle, policy)
        .expect("the real discord component must compile and validate against its manifest/policy");

    // Proves the component exports `ryuzi:gateway/gateway` (the world the
    // daemon's gateway supervisor looks for).
    assert!(
        compiled.exports_gateway(),
        "the discord component must export ryuzi:gateway/gateway"
    );

    let ctx = Arc::new(PluginCapabilityContext {
        plugin_id: "discord".to_string(),
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
        oauth_profile_ids: vec![],
    });

    // THE key assertion: instantiation succeeds — proving all four capability
    // imports (websocket/http/settings/storage) link and the `ryuzi:gateway`
    // world exports validate. Deliberately never call `start()` — that needs
    // live network and is out of scope for this de-risking proof.
    compiled.instantiate(ctx).await.expect(
        "the real discord.wasm must instantiate through ComponentRuntime with websocket linked",
    );

    // Belt-and-suspenders: the manifest we compiled against is the same one
    // the artifact builder signed.
    assert_eq!(discord_manifest().id, "discord");
}
