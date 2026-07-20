//! Remote plugin catalog: fetch + verify + cache a signed integration feed,
//! so new/updated catalog entries ship without a binary release. See
//! docs/superpowers/specs/2026-07-11-remote-plugin-catalog-design.md.

use anyhow::Context;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::task::JoinHandle;

use super::catalog_feed_key::CATALOG_FEED_PUBKEY;
use crate::control::ControlPlane;
use crate::plugins::bundle::ComponentBundleInstaller;
use crate::settings::SettingsStore;
use crate::store::{ComponentPluginReleaseRecord, RemoteCatalogRow, Store};
use ryuzi_plugin_sdk::{PluginBundleManifest, PluginManifest, PluginRelease};

/// Default feed location: the `catalog.json` asset attached to the latest
/// GitHub release. Overridable via settings for self-hosted feeds.
pub const DEFAULT_CATALOG_FEED_URL: &str =
    "https://github.com/alfin-efendy/ryuzi/releases/latest/download/catalog.json";
/// Default cadence between background feed fetches (6 hours).
pub const DEFAULT_CATALOG_FETCH_INTERVAL_MS: u64 = 21_600_000;

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogFeed {
    pub schema_version: u32,
    pub sequence: u64,
    #[serde(default)]
    pub generated_at: i64,
    #[serde(default)]
    pub entries: Vec<CatalogFeedEntry>,
    #[serde(default)]
    pub blocked: Vec<CatalogBlockedEntry>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogFeedEntry {
    pub id: String,
    pub manifest_toml: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CatalogBlockedEntry {
    pub id: String,
    #[serde(default)]
    pub reason: String,
    #[serde(default)]
    pub since_sequence: u64,
}

#[derive(Debug)]
pub enum CatalogFeedError {
    BadSignature,
    ParseError(String),
    UnsupportedSchema(u32),
    Rollback { got: u64, have: u64 },
}

/// Verify a detached ed25519 signature over `feed_bytes` against `pubkey` —
/// the single choke point every production and test verify path funnels
/// through.
///
/// Two hard rejections guard the placeholder key. The compiled-in
/// `CATALOG_FEED_PUBKEY` is all-zero, which is a valid *low-order* Edwards
/// point; non-strict ed25519 `verify()` does NOT reject low-order public keys,
/// so an attacker with no private key could forge a `(feed_bytes, signature)`
/// pair it accepts. We therefore (1) reject the all-zero key outright — so an
/// accidental future revert to the placeholder can never reintroduce
/// forgeability, independent of the strict-verify property — and (2) use
/// `verify_strict`, which rejects low-order `A` and non-canonical `R`/`S`.
/// Legitimate full-order signatures still verify.
fn verify_with(feed_bytes: &[u8], sig_bytes: &[u8], pubkey: &[u8; 32]) -> bool {
    use ed25519_dalek::{Signature, VerifyingKey};
    if pubkey == &[0u8; 32] {
        return false;
    }
    let Ok(vk) = VerifyingKey::from_bytes(pubkey) else {
        return false;
    };
    let Ok(sig_arr) = <[u8; 64]>::try_from(sig_bytes) else {
        return false;
    };
    let sig = Signature::from_bytes(&sig_arr);
    vk.verify_strict(feed_bytes, &sig).is_ok()
}

/// Verify the detached signature over `feed_bytes`, then parse, then enforce
/// schema + anti-rollback. Returns the parsed feed only when fully trusted.
/// Takes an explicit `pubkey` so tests can sign with a throwaway keypair;
/// production threads the compiled-in `CATALOG_FEED_PUBKEY`.
fn parse_and_check_with(
    feed_bytes: &[u8],
    sig_bytes: &[u8],
    last_sequence: u64,
    pubkey: &[u8; 32],
) -> Result<CatalogFeed, CatalogFeedError> {
    if !verify_with(feed_bytes, sig_bytes, pubkey) {
        return Err(CatalogFeedError::BadSignature);
    }
    let feed: CatalogFeed = serde_json::from_slice(feed_bytes)
        .map_err(|e| CatalogFeedError::ParseError(e.to_string()))?;
    if feed.schema_version != 1 {
        return Err(CatalogFeedError::UnsupportedSchema(feed.schema_version));
    }
    if feed.sequence < last_sequence {
        return Err(CatalogFeedError::Rollback {
            got: feed.sequence,
            have: last_sequence,
        });
    }
    Ok(feed)
}

/// HTTP GET seam for feed fetching, so tests can inject canned responses
/// without a real network call. A non-2xx status is a returned value, not an
/// error — only transport-level failures (DNS, connect, timeout, ...) are
/// `Err`.
#[async_trait::async_trait]
pub trait CatalogHttp: Send + Sync {
    /// GET `url`; returns `(status, body)`. Non-2xx is a returned status, not
    /// an error.
    async fn get(&self, url: &str) -> anyhow::Result<(u16, Vec<u8>)>;
}

/// Real `CatalogHttp` backed by `reqwest`.
pub struct ReqwestCatalogHttp {
    client: reqwest::Client,
}

impl ReqwestCatalogHttp {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::new(),
        }
    }
}

impl Default for ReqwestCatalogHttp {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl CatalogHttp for ReqwestCatalogHttp {
    async fn get(&self, url: &str) -> anyhow::Result<(u16, Vec<u8>)> {
        let resp = self
            .client
            .get(url)
            .header("User-Agent", "ryuzi")
            .send()
            .await?;
        let status = resp.status().as_u16();
        let bytes = resp.bytes().await?.to_vec();
        Ok((status, bytes))
    }
}

/// Outcome of a single feed fetch attempt. Never signals failure via `Err`;
/// callers (the cadence manager, doctor, RPC) inspect `applied` + `message`.
#[derive(Debug, Clone)]
pub struct FetchOutcome {
    pub applied: bool,
    pub sequence: u64,
    pub entries: usize,
    pub blocked: usize,
    pub message: String,
}

/// Fetch, verify, validate, and cache the signed remote catalog feed at
/// `feed_url`, using the production `CATALOG_FEED_PUBKEY`.
pub async fn fetch_and_cache(
    store: &Store,
    http: &dyn CatalogHttp,
    feed_url: &str,
) -> FetchOutcome {
    fetch_and_cache_with(store, http, feed_url, &CATALOG_FEED_PUBKEY).await
}

/// Persist a non-applied fetch outcome and build the `applied: false` result.
/// Re-writes `last_sequence` (never 0) so the anti-rollback counter survives a
/// transient failure, and records `outcome` — `"error"` for transport/HTTP or
/// cache-write failures, `"rejected"` for a feed that was fetched but not
/// trusted/valid — so `catalog_status.outcome` reflects the last fetch rather
/// than a stale earlier success. A persist failure is logged, not swallowed.
async fn record_failure(
    store: &Store,
    last_sequence: u64,
    outcome: &str,
    message: String,
) -> FetchOutcome {
    if let Err(e) = store.set_catalog_feed_state(last_sequence, outcome).await {
        tracing::warn!("catalog feed: failed to persist fetch outcome {outcome:?}: {e}");
    }
    FetchOutcome {
        applied: false,
        sequence: 0,
        entries: 0,
        blocked: 0,
        message,
    }
}

/// `fetch_and_cache` with an injectable verify key, so tests can sign with a
/// throwaway keypair instead of the (placeholder, all-zero) real one.
///
/// Never panics and never propagates fetch/parse/validation failures as an
/// `Err` — every failure path returns `FetchOutcome { applied: false, .. }`
/// with a human-readable `message` and persists a fetch-outcome label via
/// [`record_failure`].
async fn fetch_and_cache_with(
    store: &Store,
    http: &dyn CatalogHttp,
    feed_url: &str,
    pubkey: &[u8; 32],
) -> FetchOutcome {
    // Last-accepted sequence, read up front so a *failed* fetch can persist an
    // outcome label without clobbering the anti-rollback counter — the failure
    // paths re-persist `last` (never 0), so a transient 404/bad-sig can never
    // reset rollback protection.
    let last = store.get_catalog_feed_sequence().await.unwrap_or(0);

    let sig_url = format!("{feed_url}.sig");
    let feed_bytes = match http.get(feed_url).await {
        Ok((s, b)) if (200..300).contains(&s) => b,
        Ok((s, _)) => return record_failure(store, last, "error", format!("feed HTTP {s}")).await,
        Err(e) => {
            return record_failure(store, last, "error", format!("feed fetch failed: {e}")).await
        }
    };
    let sig_bytes = match http.get(&sig_url).await {
        Ok((s, b)) if (200..300).contains(&s) => b,
        Ok((s, _)) => {
            return record_failure(store, last, "error", format!("signature HTTP {s}")).await
        }
        Err(e) => {
            return record_failure(store, last, "error", format!("signature fetch failed: {e}"))
                .await
        }
    };

    let feed = match parse_and_check_with(&feed_bytes, &sig_bytes, last, pubkey) {
        Ok(f) => f,
        Err(e) => {
            return record_failure(store, last, "rejected", format!("feed rejected: {e:?}")).await
        }
    };

    // Validate each entry's manifest via the SDK; drop+log invalid ones.
    let now = crate::paths::now_ms();
    let mut rows: Vec<RemoteCatalogRow> = Vec::new();
    for e in &feed.entries {
        match PluginManifest::from_toml(&e.manifest_toml) {
            Ok(m) => rows.push(RemoteCatalogRow {
                id: e.id.clone(),
                manifest_toml: e.manifest_toml.clone(),
                version: m.version.clone(),
                sequence: feed.sequence,
                blocked: false,
                blocked_reason: None,
                fetched_at: now,
            }),
            Err(err) => tracing::warn!("catalog feed: dropping invalid entry {}: {err}", e.id),
        }
    }
    // Blocked entries: merge into an existing row if present, else record as
    // a standalone blocked row (id may or may not also be a valid entry).
    for b in &feed.blocked {
        if let Some(row) = rows.iter_mut().find(|r| r.id == b.id) {
            row.blocked = true;
            row.blocked_reason = Some(b.reason.clone());
        } else {
            rows.push(RemoteCatalogRow {
                id: b.id.clone(),
                manifest_toml: String::new(),
                version: String::new(),
                sequence: feed.sequence,
                blocked: true,
                blocked_reason: Some(b.reason.clone()),
                fetched_at: now,
            });
        }
    }

    let entries = rows.iter().filter(|r| !r.blocked).count();
    let blocked = rows.iter().filter(|r| r.blocked).count();
    if let Err(e) = store.upsert_remote_catalog(&rows).await {
        return record_failure(store, last, "error", format!("cache write failed: {e}")).await;
    }
    if let Err(e) = store.set_catalog_feed_state(feed.sequence, "ok").await {
        tracing::warn!(
            "catalog feed: failed to persist accepted sequence {}: {e}",
            feed.sequence
        );
    }
    FetchOutcome {
        applied: true,
        sequence: feed.sequence,
        entries,
        blocked,
        message: format!(
            "applied sequence {} ({entries} entries, {blocked} blocked)",
            feed.sequence
        ),
    }
}

/// The content-relevant projection of a cached catalog row — everything a
/// consumer actually reacts to (`id`, manifest, version, block state),
/// deliberately excluding `fetched_at` (a wall-clock stamp refreshed on
/// every upsert) and `sequence` (the feed's monotonic counter, which moves
/// even when the merged set is byte-identical). Comparing these projections
/// is how `refresh_verified` decides whether the *effective* catalog changed
/// — matching the design doc's "changed = new/removed/version-changed/blocked
/// entries", not raw-row equality. `list_remote_catalog` already returns rows
/// `ORDER BY id`, so a plain `Vec` comparison is order-stable.
pub(crate) type CatalogContent = (String, String, String, bool, Option<String>);

/// Project cached rows down to their content-relevant fields — the same
/// comparison basis [`RemoteCatalogManager::refresh_verified`] uses to decide
/// whether the effective catalog changed. `pub(crate)` so the `refresh_catalog`
/// RPC handler (`crate::api::remote_catalog_api`) can reuse this instead of
/// re-deriving the projection and risking drift between the two change
/// detectors.
pub(crate) fn catalog_content(rows: &[RemoteCatalogRow]) -> Vec<CatalogContent> {
    rows.iter()
        .map(|r| {
            (
                r.id.clone(),
                r.manifest_toml.clone(),
                r.version.clone(),
                r.blocked,
                r.blocked_reason.clone(),
            )
        })
        .collect()
}

/// Periodic-fetch cadence for the remote plugin catalog: mirrors
/// `crate::update::manager::UpdateManager`'s timer shape exactly (initial
/// fetch on boot, then a fixed interval), so the two background loops read
/// the same way. Owns no long-lived lock beyond the `JoinHandle` itself —
/// each `refresh` verifies and re-caches the feed via `fetch_and_cache_with`.
pub struct RemoteCatalogManager {
    store: Arc<Store>,
    settings: SettingsStore,
    cp: Arc<ControlPlane>,
    http: Arc<dyn CatalogHttp>,
    timer: Mutex<Option<JoinHandle<()>>>,
}

impl RemoteCatalogManager {
    pub fn new(
        store: Arc<Store>,
        settings: SettingsStore,
        cp: Arc<ControlPlane>,
        http: Arc<dyn CatalogHttp>,
    ) -> Arc<Self> {
        Arc::new(Self {
            store,
            settings,
            cp,
            http,
            timer: Mutex::new(None),
        })
    }

    async fn feed_url(&self) -> String {
        self.settings
            .get("catalog_feed_url")
            .await
            .ok()
            .flatten()
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| DEFAULT_CATALOG_FEED_URL.to_string())
    }

    async fn interval_ms(&self) -> u64 {
        self.settings
            .get("catalog_fetch_interval_ms")
            .await
            .ok()
            .flatten()
            .and_then(|v| v.parse().ok())
            .unwrap_or(DEFAULT_CATALOG_FETCH_INTERVAL_MS)
    }

    /// Fetch once, apply, and — if the effective cached set changed — set
    /// the restart-required flag so Cockpit/the CLI can prompt a reload.
    /// Verifies against the production `CATALOG_FEED_PUBKEY`.
    pub async fn refresh(&self) -> FetchOutcome {
        self.refresh_verified(&CATALOG_FEED_PUBKEY).await
    }

    /// `refresh`, but verifying against an injected key instead of the
    /// compiled-in `CATALOG_FEED_PUBKEY` — a test-only seam, since that key
    /// is currently an all-zero placeholder (see `catalog_feed_key`) that no
    /// test-signed feed can satisfy.
    #[cfg(test)]
    async fn refresh_with_pubkey(&self, pubkey: &[u8; 32]) -> FetchOutcome {
        self.refresh_verified(pubkey).await
    }

    async fn refresh_verified(&self, pubkey: &[u8; 32]) -> FetchOutcome {
        let url = self.feed_url().await;
        let before = catalog_content(&self.store.list_remote_catalog().await.unwrap_or_default());
        let outcome = fetch_and_cache_with(&self.store, self.http.as_ref(), &url, pubkey).await;
        if outcome.applied {
            let after =
                catalog_content(&self.store.list_remote_catalog().await.unwrap_or_default());
            // Only the *effective* catalog changing warrants a restart prompt.
            // Comparing content projections (not whole rows) avoids flipping the
            // flag every cycle on `fetched_at`/`sequence` churn from a re-fetch
            // of byte-identical content.
            if after != before {
                self.cp.mark_plugins_restart_required();
            }
            let _ = crate::plugins::apply_blocked_denylist(
                &self.store,
                &self.settings,
                self.cp.plugins(),
            )
            .await;
        }
        outcome
    }

    /// Initial fetch on boot, then a real interval loop — mirrors
    /// `UpdateManager::start` verbatim (see that method's doc): the fetch
    /// itself is `refresh`'s job to keep best-effort, so this task body can
    /// never panic and never needs its own error handling.
    pub fn start(self: &Arc<Self>) {
        let me = Arc::clone(self);
        let handle = tokio::spawn(async move {
            me.refresh().await; // initial fetch on boot
            let ms = me.interval_ms().await.max(1);
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(ms));
            interval.tick().await; // consume the immediate first tick
            loop {
                interval.tick().await;
                me.refresh().await;
            }
        });
        *self.timer.lock().unwrap() = Some(handle);
    }

    /// Aborts the timer task; safe to call when no timer is armed.
    pub fn stop(&self) {
        if let Some(h) = self.timer.lock().unwrap().take() {
            h.abort();
        }
    }
}

// ===========================================================================
// Component-plugin release resolution + install pipeline (Task 11a).
//
// The catalog *feed* above advertises which integrations exist (manifest
// data). This half turns a plugin id (+ optional pinned version) into the
// concrete, SIGNED component-release artifacts, downloads and STAGES them, and
// runs them through `plugins::bundle::verify_bundle` before activation — the
// catalog is treated as data, never trusted to name code that bypasses
// signature verification.
//
// ## Release feed layout (convention — swappable, documented for units 11b/12)
// Given a base URL (the settings `component_release_base_url`, default
// [`DEFAULT_COMPONENT_RELEASE_BASE_URL`]) and a `stem` of `<id>` (latest) or
// `<id>-<version>` (pinned), four artifacts make up one bundle:
//   - `<base>/<stem>.ryuzi-plugin.toml`     -> staged as `ryuzi-plugin.toml`
//   - `<base>/<stem>.release.json`          -> staged as `release.json`
//   - `<base>/<stem>.release.json.sig`      -> staged as `plugin.sig`
//   - the `component_url` INSIDE release.json -> staged as `<manifest.component>`
// These four staged files are exactly what `verify_bundle` expects. Unit 11b's
// signer/build script MUST publish them at these names; Task 12's UI resolves
// installs through the RPCs in `api::plugins_api`, never this URL scheme
// directly. Only the release.json's *raw fetched bytes* are what the signature
// is verified over (see `plugins::bundle`), so they are staged byte-for-byte.
// ===========================================================================

/// Default location component release artifacts are fetched from: the same
/// GitHub "latest release download" host the catalog feed uses. Overridable
/// via the `component_release_base_url` setting for self-hosted registries.
pub const DEFAULT_COMPONENT_RELEASE_BASE_URL: &str =
    "https://github.com/alfin-efendy/ryuzi/releases/latest/download";

/// Settings key that, once present, marks first-party component bootstrap as
/// fully completed (every first-party bundle installed or already present).
/// Mirrors `agents::bootstrap`'s `FREE_PROVIDERS_SEEDED_MARKER`: once set, a
/// later user uninstall is respected and bootstrap never re-runs. It stays
/// ABSENT while any bundle is still missing, so a transient failure is retried
/// on the next boot.
pub const FIRST_PARTY_BOOTSTRAP_MARKER: &str = "first_party_components_bootstrapped_v1";

/// Settings key holding a human-readable retry message when the last bootstrap
/// attempt landed NOTHING (every download/verify failed). Read by the
/// `component_bootstrap_status` RPC so Cockpit can show a retry banner; cleared
/// once bootstrap fully completes.
pub const FIRST_PARTY_BOOTSTRAP_RETRY: &str = "first_party_components_bootstrap_retry";

/// The first-party component bundle ids the daemon bootstraps on first run.
pub const FIRST_PARTY_BUNDLE_IDS: &[&str] = &["mimo", "opencode"];

/// `<id>` for a latest install, `<id>-<version>` for a pinned one.
fn release_stem(plugin_id: &str, version: Option<&str>) -> String {
    match version {
        Some(v) => format!("{plugin_id}-{v}"),
        None => plugin_id.to_string(),
    }
}

/// GET `url` through the injectable seam and require a 2xx, returning the body.
/// A non-2xx status and a transport error both become an `Err` here (unlike the
/// feed fetch, which classifies them) — the install pipeline's caller decides
/// whether a failure is fatal or best-effort.
async fn get_2xx(http: &dyn CatalogHttp, url: &str) -> anyhow::Result<Vec<u8>> {
    match http.get(url).await {
        Ok((status, body)) if (200..300).contains(&status) => Ok(body),
        Ok((status, _)) => anyhow::bail!("HTTP {status} fetching {url}"),
        Err(error) => Err(anyhow::anyhow!("fetching {url}: {error}")),
    }
}

/// Resolve, download, stage, VERIFY, and install+activate one signed component
/// release for `plugin_id` (optionally pinned to `version`). Fetches the four
/// bundle artifacts (see this section's doc), stages them into a throwaway
/// dir, runs [`crate::plugins::bundle::verify_bundle`] against `trusted_keys`,
/// and hands the result to `installer.install_verified` (which performs the DB
/// writes and self-rolls-back on failure). Returns the installed, active
/// release record on success.
///
/// Every step is a hard error: a bad HTTP status, a malformed descriptor, an
/// id/version mismatch, a failed signature/hash check, or an install failure
/// all propagate as `Err`. Callers that want best-effort behavior (the
/// bootstrap below) catch it.
pub async fn install_component_release(
    http: &dyn CatalogHttp,
    installer: &ComponentBundleInstaller,
    trusted_keys: &HashMap<String, [u8; 32]>,
    base_url: &str,
    plugin_id: &str,
    version: Option<&str>,
) -> anyhow::Result<ComponentPluginReleaseRecord> {
    let base = base_url.trim_end_matches('/');
    let stem = release_stem(plugin_id, version);
    let manifest_url = format!("{base}/{stem}.ryuzi-plugin.toml");
    let release_url = format!("{base}/{stem}.release.json");
    let sig_url = format!("{release_url}.sig");

    // Fetch the three descriptor files. release.json's RAW bytes are what the
    // signature is verified over, so we stage exactly what we fetched.
    let manifest_bytes = get_2xx(http, &manifest_url).await?;
    let release_bytes = get_2xx(http, &release_url).await?;
    let sig_bytes = get_2xx(http, &sig_url).await?;

    // Parse (and structurally validate) both descriptors up front — before the
    // (larger) wasm download — so a malformed feed fails fast, and to learn the
    // component filename to stage the wasm under. `verify_bundle` re-parses and
    // re-checks everything; this parse only drives staging.
    let manifest = PluginBundleManifest::from_toml(
        std::str::from_utf8(&manifest_bytes).context("fetched ryuzi-plugin.toml is not UTF-8")?,
    )
    .context("parsing fetched ryuzi-plugin.toml")?;
    let release =
        PluginRelease::from_json(&release_bytes).context("parsing fetched release.json")?;
    if manifest.id != plugin_id || release.id != plugin_id {
        anyhow::bail!(
            "release feed id mismatch: requested {plugin_id}, got manifest {:?} / release {:?}",
            manifest.id,
            release.id
        );
    }
    if let Some(v) = version {
        if release.version != v {
            anyhow::bail!(
                "release feed version mismatch: requested {v}, got {}",
                release.version
            );
        }
    }

    // Download the component wasm named by the release.
    let component_bytes = get_2xx(http, &release.component_url).await?;

    // Stage all four bundle files into a throwaway dir. On a successful install
    // the dir is renamed into place; the `TempDir` guard's drop then removes a
    // path that no longer exists, which is a harmless no-op. On any failure
    // before that, the guard cleans the staging dir up.
    let staging = tempfile::tempdir().context("creating component staging dir")?;
    let staging_path = staging.path();
    std::fs::write(staging_path.join("ryuzi-plugin.toml"), &manifest_bytes)?;
    std::fs::write(staging_path.join("release.json"), &release_bytes)?;
    std::fs::write(staging_path.join("plugin.sig"), &sig_bytes)?;
    // Stage the wasm under the exact filename the manifest names; verify_bundle
    // resolves and canonicalizes `manifest.component` against the staging root.
    let component_dest = staging_path.join(&manifest.component);
    if let Some(parent) = component_dest.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(&component_dest, &component_bytes)?;

    let verified = crate::plugins::bundle::verify_bundle(staging_path, trusted_keys)
        .context("verifying staged component bundle")?;
    let record = installer.install_verified(verified).await?;
    Ok(record)
}

/// Outcome of a [`bootstrap_first_party_components`] pass. Best-effort: never
/// an `Err`. The daemon discards this (it only needs the side effects); it is
/// returned so the pipeline can be unit-tested directly.
#[derive(Debug, Clone, Default)]
pub struct FirstPartyBootstrapOutcome {
    /// True if this pass actually tried to install anything (marker absent and
    /// a real trusted key present).
    pub attempted: bool,
    /// Plugin ids installed by THIS pass.
    pub installed: Vec<String>,
    /// Plugin ids whose install failed this pass.
    pub failed: Vec<String>,
    /// True when every first-party bundle is satisfied (installed now or
    /// already active) — the completion marker was set.
    pub complete: bool,
    /// True when nothing landed and a retryable status was recorded.
    pub retry_pending: bool,
}

/// Best-effort first-run bootstrap of the first-party component bundles.
/// Idempotent and respectful of user deletion via [`FIRST_PARTY_BOOTSTRAP_MARKER`]
/// (mirrors `ensure_free_providers_seeded`). Never returns an `Err`: the daemon
/// wires this as warn-and-continue so a download failure never fails
/// `build_daemon`.
///
/// Behavior:
/// - Marker already set → no-op (respects a prior user uninstall).
/// - `trusted_keys` empty (the all-zero first-party placeholder) → no-op: with
///   no key nothing can ever verify, so it touches neither the network nor any
///   retry state; the feature is simply disabled until the real key ships.
/// - Otherwise it ATTEMPTS each id independently (a prior partial install is
///   detected via `active_component_release` and counted, never re-installed).
///   Every id satisfied → sets the completion marker + clears retry state.
///   Nothing landed → records a retryable status for the doctor RPC.
pub async fn bootstrap_first_party_components(
    store: &Store,
    http: &dyn CatalogHttp,
    trusted_keys: &HashMap<String, [u8; 32]>,
    installer: &ComponentBundleInstaller,
    base_url: &str,
) -> FirstPartyBootstrapOutcome {
    let mut outcome = FirstPartyBootstrapOutcome::default();

    if store
        .get_setting_raw(FIRST_PARTY_BOOTSTRAP_MARKER)
        .await
        .ok()
        .flatten()
        .is_some()
    {
        outcome.complete = true;
        return outcome;
    }
    if trusted_keys.is_empty() {
        return outcome;
    }

    outcome.attempted = true;
    let mut satisfied = 0usize;
    let mut installed_this_pass = false;
    for id in FIRST_PARTY_BUNDLE_IDS {
        // A prior partial boot may have installed this one already; count it as
        // satisfied and never re-install (install_verified rejects a duplicate
        // version directory).
        if matches!(
            store.active_component_release(id).await,
            Ok(Some(rec)) if !rec.revoked
        ) {
            satisfied += 1;
            continue;
        }
        match install_component_release(http, installer, trusted_keys, base_url, id, None).await {
            Ok(rec) => {
                installed_this_pass = true;
                satisfied += 1;
                outcome.installed.push(rec.plugin_id);
            }
            Err(error) => {
                tracing::warn!("first-party bootstrap: installing {id} failed: {error:#}");
                outcome.failed.push((*id).to_string());
            }
        }
    }

    if satisfied == FIRST_PARTY_BUNDLE_IDS.len() {
        outcome.complete = true;
        if let Err(error) = store
            .set_setting_raw(FIRST_PARTY_BOOTSTRAP_MARKER, "1")
            .await
        {
            tracing::warn!("first-party bootstrap: persisting completion marker failed: {error}");
        }
        let _ = store.delete_setting_raw(FIRST_PARTY_BOOTSTRAP_RETRY).await;
    } else if !installed_this_pass {
        // Nothing landed this attempt — record a retryable status Cockpit can
        // surface. The daemon does not fail; the next boot retries.
        outcome.retry_pending = true;
        let message = format!(
            "Could not download the first-party plugins ({}). They will be retried automatically.",
            outcome.failed.join(", ")
        );
        if let Err(error) = store
            .set_setting_raw(FIRST_PARTY_BOOTSTRAP_RETRY, &message)
            .await
        {
            tracing::warn!("first-party bootstrap: persisting retry status failed: {error}");
        }
    }
    outcome
}

#[cfg(test)]
mod component_install_tests {
    use super::*;
    use crate::plugins::bundle::ComponentBundleInstaller;
    use ed25519_dalek::{Signer, SigningKey};
    use sha2::Digest;
    use std::sync::Mutex as StdMutex;

    const KEY_ID: &str = "first-party";

    fn bundle_key() -> SigningKey {
        SigningKey::from_bytes(&[11u8; 32])
    }

    fn trusted() -> HashMap<String, [u8; 32]> {
        HashMap::from([(KEY_ID.to_string(), bundle_key().verifying_key().to_bytes())])
    }

    fn b64url(bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }

    /// The four release artifacts for one `(id, version)` bundle.
    struct Artifacts {
        manifest_toml: Vec<u8>,
        release_json: Vec<u8>,
        sig_json: Vec<u8>,
        wasm: Vec<u8>,
        component_url: String,
    }

    /// Build a fully valid, signed set of the four artifacts for `(id,
    /// version)` under `key`/`key_id`.
    fn build_artifacts(id: &str, version: &str, key: &SigningKey, key_id: &str) -> Artifacts {
        let wasm = format!("wasm bytes for {id} {version}").into_bytes();
        let sha = format!("{:x}", sha2::Sha256::digest(&wasm));
        let component = format!("{id}.wasm");
        let component_url = format!("http://feed.test/{id}-{version}/{component}");
        let manifest_toml = format!(
            "id = \"{id}\"\nname = \"{id}\"\nversion = \"{version}\"\nwit-api = \"^0.1.0\"\nlifecycle = \"singleton\"\ncomponent = \"{component}\"\n"
        )
        .into_bytes();
        let release_json = format!(
            "{{\"id\":\"{id}\",\"version\":\"{version}\",\"wit-api\":\"0.1.0\",\"component_url\":\"{component_url}\",\"component_sha256\":\"{sha}\"}}"
        )
        .into_bytes();
        let signature = key.sign(&release_json);
        let sig_json = serde_json::to_vec(&serde_json::json!({
            "key_id": key_id,
            "signature": b64url(&signature.to_bytes()),
        }))
        .unwrap();
        Artifacts {
            manifest_toml,
            release_json,
            sig_json,
            wasm,
            component_url,
        }
    }

    /// A `CatalogHttp` fake that serves canned bodies keyed by exact URL; any
    /// unregistered URL is a 404.
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
        /// Register all four artifacts of a latest (unpinned) install at `base`.
        fn register_latest(&self, base: &str, id: &str, a: &Artifacts) {
            self.put(
                format!("{base}/{id}.ryuzi-plugin.toml"),
                200,
                a.manifest_toml.clone(),
            );
            self.put(
                format!("{base}/{id}.release.json"),
                200,
                a.release_json.clone(),
            );
            self.put(
                format!("{base}/{id}.release.json.sig"),
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

    async fn test_store() -> (Store, tempfile::NamedTempFile) {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        (store, tmp)
    }

    #[tokio::test]
    async fn install_pipeline_verifies_and_activates() {
        let (store, _tmp) = test_store().await;
        let root = tempfile::tempdir().unwrap();
        let installer = ComponentBundleInstaller::new(root.path().to_path_buf(), store.clone());
        let base = "http://feed.test/latest";
        let http = FakeReleaseHttp::new();
        http.register_latest(
            base,
            "mimo",
            &build_artifacts("mimo", "0.1.0", &bundle_key(), KEY_ID),
        );

        let record = install_component_release(&http, &installer, &trusted(), base, "mimo", None)
            .await
            .unwrap();
        assert_eq!(record.plugin_id, "mimo");
        assert_eq!(record.version, "0.1.0");
        assert!(record.active);
        assert_eq!(
            store
                .active_component_release("mimo")
                .await
                .unwrap()
                .unwrap()
                .version,
            "0.1.0"
        );
    }

    #[tokio::test]
    async fn install_pipeline_rejects_untrusted_key() {
        let (store, _tmp) = test_store().await;
        let root = tempfile::tempdir().unwrap();
        let installer = ComponentBundleInstaller::new(root.path().to_path_buf(), store.clone());
        let base = "http://feed.test/latest";
        let http = FakeReleaseHttp::new();
        // Signed by a key whose id is not in the trusted map.
        let rogue = SigningKey::from_bytes(&[5u8; 32]);
        http.register_latest(
            base,
            "mimo",
            &build_artifacts("mimo", "0.1.0", &rogue, "rogue"),
        );

        let err = install_component_release(&http, &installer, &trusted(), base, "mimo", None)
            .await
            .expect_err("an untrusted signing key must be rejected");
        assert!(
            err.to_string().contains("untrusted signing key")
                || format!("{err:#}").contains("untrusted signing key"),
            "unexpected error: {err:#}"
        );
        assert!(store
            .active_component_release("mimo")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn install_pipeline_rejects_tampered_component() {
        let (store, _tmp) = test_store().await;
        let root = tempfile::tempdir().unwrap();
        let installer = ComponentBundleInstaller::new(root.path().to_path_buf(), store.clone());
        let base = "http://feed.test/latest";
        let http = FakeReleaseHttp::new();
        let a = build_artifacts("mimo", "0.1.0", &bundle_key(), KEY_ID);
        http.register_latest(base, "mimo", &a);
        // Serve a different wasm than the one the release hashed — hash mismatch.
        http.put(a.component_url.clone(), 200, b"tampered wasm".to_vec());

        let err = install_component_release(&http, &installer, &trusted(), base, "mimo", None)
            .await
            .expect_err("a tampered component must fail the hash check");
        assert!(
            format!("{err:#}").contains("hash mismatch"),
            "unexpected error: {err:#}"
        );
    }

    async fn installer_for(store: &Store) -> (ComponentBundleInstaller, tempfile::TempDir) {
        let root = tempfile::tempdir().unwrap();
        let installer = ComponentBundleInstaller::new(root.path().to_path_buf(), store.clone());
        (installer, root)
    }

    #[tokio::test]
    async fn bootstrap_installs_both_and_sets_marker() {
        let (store, _tmp) = test_store().await;
        let (installer, _root) = installer_for(&store).await;
        let base = "http://feed.test/latest";
        let http = FakeReleaseHttp::new();
        for id in FIRST_PARTY_BUNDLE_IDS {
            http.register_latest(
                base,
                id,
                &build_artifacts(id, "0.1.0", &bundle_key(), KEY_ID),
            );
        }

        let outcome =
            bootstrap_first_party_components(&store, &http, &trusted(), &installer, base).await;
        assert!(outcome.attempted);
        assert!(outcome.complete);
        assert!(outcome.installed.contains(&"mimo".to_string()));
        assert!(outcome.installed.contains(&"opencode".to_string()));
        assert!(store
            .active_component_release("mimo")
            .await
            .unwrap()
            .is_some());
        assert!(store
            .active_component_release("opencode")
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get_setting_raw(FIRST_PARTY_BOOTSTRAP_MARKER)
            .await
            .unwrap()
            .is_some());
        assert!(store
            .get_setting_raw(FIRST_PARTY_BOOTSTRAP_RETRY)
            .await
            .unwrap()
            .is_none());
    }

    // One id failing must not block the other: the successful release is
    // recorded independently, and (since something DID land) no retry banner.
    #[tokio::test]
    async fn bootstrap_records_each_success_independently() {
        let (store, _tmp) = test_store().await;
        let (installer, _root) = installer_for(&store).await;
        let base = "http://feed.test/latest";
        let http = FakeReleaseHttp::new();
        // Only mimo is served; opencode's URLs 404.
        http.register_latest(
            base,
            "mimo",
            &build_artifacts("mimo", "0.1.0", &bundle_key(), KEY_ID),
        );

        let outcome =
            bootstrap_first_party_components(&store, &http, &trusted(), &installer, base).await;
        assert!(outcome.attempted);
        assert!(!outcome.complete, "not every id landed");
        assert_eq!(outcome.installed, vec!["mimo".to_string()]);
        assert_eq!(outcome.failed, vec!["opencode".to_string()]);
        assert!(store
            .active_component_release("mimo")
            .await
            .unwrap()
            .is_some());
        assert!(store
            .active_component_release("opencode")
            .await
            .unwrap()
            .is_none());
        // Marker unset (retry the missing one next boot); no retry banner since
        // one bundle did land this pass.
        assert!(store
            .get_setting_raw(FIRST_PARTY_BOOTSTRAP_MARKER)
            .await
            .unwrap()
            .is_none());
        assert!(!outcome.retry_pending);
    }

    #[tokio::test]
    async fn bootstrap_both_fail_records_retryable_status() {
        let (store, _tmp) = test_store().await;
        let (installer, _root) = installer_for(&store).await;
        let base = "http://feed.test/latest";
        // Nothing registered — every fetch 404s.
        let http = FakeReleaseHttp::new();

        let outcome =
            bootstrap_first_party_components(&store, &http, &trusted(), &installer, base).await;
        assert!(outcome.attempted);
        assert!(!outcome.complete);
        assert!(outcome.retry_pending);
        assert!(store
            .active_component_release("mimo")
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get_setting_raw(FIRST_PARTY_BOOTSTRAP_MARKER)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get_setting_raw(FIRST_PARTY_BOOTSTRAP_RETRY)
            .await
            .unwrap()
            .is_some());
    }

    #[tokio::test]
    async fn bootstrap_is_noop_when_marker_present() {
        let (store, _tmp) = test_store().await;
        let (installer, _root) = installer_for(&store).await;
        let base = "http://feed.test/latest";
        let http = FakeReleaseHttp::new();
        for id in FIRST_PARTY_BUNDLE_IDS {
            http.register_latest(
                base,
                id,
                &build_artifacts(id, "0.1.0", &bundle_key(), KEY_ID),
            );
        }
        store
            .set_setting_raw(FIRST_PARTY_BOOTSTRAP_MARKER, "1")
            .await
            .unwrap();

        let outcome =
            bootstrap_first_party_components(&store, &http, &trusted(), &installer, base).await;
        assert!(!outcome.attempted, "marker present must short-circuit");
        assert!(outcome.complete);
        // Nothing installed despite artifacts being available — respects a prior
        // user uninstall.
        assert!(store
            .active_component_release("mimo")
            .await
            .unwrap()
            .is_none());
    }

    #[tokio::test]
    async fn bootstrap_is_noop_with_empty_trusted_keys() {
        let (store, _tmp) = test_store().await;
        let (installer, _root) = installer_for(&store).await;
        let base = "http://feed.test/latest";
        let http = FakeReleaseHttp::new();
        for id in FIRST_PARTY_BUNDLE_IDS {
            http.register_latest(
                base,
                id,
                &build_artifacts(id, "0.1.0", &bundle_key(), KEY_ID),
            );
        }

        // Placeholder (empty) trusted set: fail-closed, no network, no state.
        let outcome =
            bootstrap_first_party_components(&store, &http, &HashMap::new(), &installer, base)
                .await;
        assert!(!outcome.attempted);
        assert!(!outcome.complete);
        assert!(!outcome.retry_pending);
        assert!(store
            .active_component_release("mimo")
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get_setting_raw(FIRST_PARTY_BOOTSTRAP_RETRY)
            .await
            .unwrap()
            .is_none());
        assert!(store
            .get_setting_raw(FIRST_PARTY_BOOTSTRAP_MARKER)
            .await
            .unwrap()
            .is_none());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    // A deterministic test keypair; the test overrides the verify key.
    fn test_keypair() -> SigningKey {
        SigningKey::from_bytes(&[7u8; 32])
    }

    fn sign(bytes: &[u8]) -> Vec<u8> {
        test_keypair().sign(bytes).to_bytes().to_vec()
    }

    fn feed_json(seq: u64) -> String {
        format!(
            r#"{{"schemaVersion":1,"sequence":{seq},"generatedAt":0,
                "entries":[{{"id":"acme","manifestToml":"contract=1\nid=\"acme\"\nname=\"Acme\"\nversion=\"1.0.0\""}}],
                "blocked":[]}}"#
        )
    }

    #[test]
    fn valid_signed_feed_parses() {
        let bytes = feed_json(5).into_bytes();
        let sig = sign(&bytes);
        let pubkey = test_keypair().verifying_key().to_bytes();
        let feed = parse_and_check_with(&bytes, &sig, 0, &pubkey).unwrap();
        assert_eq!(feed.sequence, 5);
        assert_eq!(feed.entries[0].id, "acme");
    }

    #[test]
    fn tampered_bytes_rejected() {
        let bytes = feed_json(5).into_bytes();
        let sig = sign(&bytes);
        let mut tampered = bytes.clone();
        tampered[40] ^= 0xff;
        let pubkey = test_keypair().verifying_key().to_bytes();
        assert!(matches!(
            parse_and_check_with(&tampered, &sig, 0, &pubkey),
            Err(CatalogFeedError::BadSignature)
        ));
    }

    #[test]
    fn lower_sequence_rejected_anti_rollback() {
        let bytes = feed_json(3).into_bytes();
        let sig = sign(&bytes);
        let pubkey = test_keypair().verifying_key().to_bytes();
        assert!(matches!(
            parse_and_check_with(&bytes, &sig, 5, &pubkey),
            Err(CatalogFeedError::Rollback { got: 3, have: 5 })
        ));
    }

    // The compiled-in `CATALOG_FEED_PUBKEY` is the all-zero placeholder — a
    // valid LOW-ORDER Edwards point. Non-strict ed25519 `verify()` does NOT
    // reject low-order public keys, so before this fix an attacker with no
    // private key could grind a `(bytes, signature)` pair that `verify_with`
    // accepted against it. `verify_with` now rejects the all-zero key two ways
    // (an explicit guard AND `verify_strict`), so verification against it is
    // deterministically `false` for ANY signature. This locks the fail-closed
    // property: a silent regression (a revert to the placeholder, or a swap
    // back to non-strict `verify`) can never reintroduce the forgery.
    #[test]
    fn all_zero_placeholder_key_rejects_every_signature() {
        let bytes = feed_json(5).into_bytes();
        // A perfectly valid signature under a real full-order key — still
        // rejected when checked against the all-zero placeholder.
        let valid_sig = sign(&bytes);
        assert!(!verify_with(&bytes, &valid_sig, &CATALOG_FEED_PUBKEY));
        assert!(!verify_with(&bytes, &valid_sig, &[0u8; 32]));
        // Degenerate / trivially-forged signature shapes are rejected too.
        assert!(!verify_with(&bytes, &[0u8; 64], &[0u8; 32]));
        assert!(!verify_with(&bytes, &[0xffu8; 64], &[0u8; 32]));
        // ...and the parse+check path over the production key must classify it
        // as a bad signature, never accept the feed.
        assert!(matches!(
            parse_and_check_with(&bytes, &valid_sig, 0, &CATALOG_FEED_PUBKEY),
            Err(CatalogFeedError::BadSignature)
        ));
    }

    // Counterpart to the guard test: switching `verify()` -> `verify_strict()`
    // must NOT break the legitimate path — a full-order key's own signature
    // still verifies. (The round-trip/`parse_and_check_with` tests above rely
    // on this too; assert it directly at the choke point so a broken verify
    // swap fails here, loudly.)
    #[test]
    fn full_order_key_signature_still_verifies_under_strict() {
        let bytes = feed_json(5).into_bytes();
        let sig = sign(&bytes);
        let pubkey = test_keypair().verifying_key().to_bytes();
        assert!(verify_with(&bytes, &sig, &pubkey));
    }

    struct FakeHttp {
        // url suffix -> (status, bytes)
        feed: (u16, Vec<u8>),
        sig: (u16, Vec<u8>),
    }
    #[async_trait::async_trait]
    impl CatalogHttp for FakeHttp {
        async fn get(&self, url: &str) -> anyhow::Result<(u16, Vec<u8>)> {
            if url.ends_with(".sig") {
                Ok(self.sig.clone())
            } else {
                Ok(self.feed.clone())
            }
        }
    }

    #[tokio::test]
    async fn fetch_and_cache_stores_verified_entries_and_sequence() {
        let bytes = feed_json(7).into_bytes();
        let sig = sign(&bytes);
        // NOTE: this test relies on the real CATALOG_FEED_PUBKEY matching the test
        // key; instead, fetch_and_cache takes an injected verify via a helper.
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = FakeHttp {
            feed: (200, bytes),
            sig: (200, sig),
        };
        let outcome = fetch_and_cache_with(
            &store,
            &http,
            "http://x/catalog.json",
            &test_keypair().verifying_key().to_bytes(),
        )
        .await;
        assert!(outcome.applied);
        assert_eq!(outcome.sequence, 7);
        assert_eq!(store.get_catalog_feed_sequence().await.unwrap(), 7);
        assert_eq!(store.list_remote_catalog().await.unwrap().len(), 1);
    }

    // A non-2xx feed response must warn-and-continue: no apply, no cache write,
    // sequence untouched. A periodic fetcher hitting a 404 must not crash.
    #[tokio::test]
    async fn fetch_rejects_non_2xx_feed() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = FakeHttp {
            feed: (404, vec![]),
            sig: (200, vec![]),
        };
        let outcome = fetch_and_cache_with(
            &store,
            &http,
            "http://x/catalog.json",
            &test_keypair().verifying_key().to_bytes(),
        )
        .await;
        assert!(!outcome.applied);
        assert!(store.list_remote_catalog().await.unwrap().is_empty());
        assert_eq!(store.get_catalog_feed_sequence().await.unwrap(), 0);
    }

    // A tampered feed served with the real signature must fail verification and
    // never be applied — the untrusted bytes must not reach the cache.
    #[tokio::test]
    async fn fetch_rejects_bad_signature() {
        let bytes = feed_json(7).into_bytes();
        let sig = sign(&bytes);
        let mut tampered = bytes.clone();
        tampered[40] ^= 0xff;
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = FakeHttp {
            feed: (200, tampered),
            sig: (200, sig),
        };
        let outcome = fetch_and_cache_with(
            &store,
            &http,
            "http://x/catalog.json",
            &test_keypair().verifying_key().to_bytes(),
        )
        .await;
        assert!(!outcome.applied);
        assert!(store.list_remote_catalog().await.unwrap().is_empty());
        assert_eq!(store.get_catalog_feed_sequence().await.unwrap(), 0);
    }

    // An older-sequence feed must be rejected by anti-rollback and must NOT
    // overwrite the already-accepted sequence.
    #[tokio::test]
    async fn fetch_rejects_rollback_feed_keeps_accepted_sequence() {
        let bytes = feed_json(3).into_bytes();
        let sig = sign(&bytes);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        store.set_catalog_feed_state(9, "ok").await.unwrap();
        let http = FakeHttp {
            feed: (200, bytes),
            sig: (200, sig),
        };
        let outcome = fetch_and_cache_with(
            &store,
            &http,
            "http://x/catalog.json",
            &test_keypair().verifying_key().to_bytes(),
        )
        .await;
        assert!(!outcome.applied);
        assert_eq!(store.get_catalog_feed_sequence().await.unwrap(), 9);
    }

    async fn test_cp() -> Arc<crate::control::ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        {
            let persistence = crate::agents::bootstrap::AgentPersistence::temporary(store.clone())
                .await
                .unwrap();
            crate::control::ControlPlane::new(store, crate::plugins::Registries::new(), persistence)
                .await
        }
    }

    // A fresh `ControlPlane` sharing an existing `Arc<Store>` — its
    // in-memory restart flag always starts false, letting a second refresh
    // over the same populated cache assert the flag stays down.
    async fn test_cp_over(store: Arc<crate::store::Store>) -> Arc<crate::control::ControlPlane> {
        let persistence = crate::agents::bootstrap::AgentPersistence::temporary(store.clone())
            .await
            .unwrap();
        crate::control::ControlPlane::new_with_telemetry(
            store,
            crate::plugins::Registries::new(),
            Arc::new(crate::telemetry::NoopTelemetry),
            persistence,
        )
        .await
    }

    // `refresh` must fetch+apply a changed feed and, because the cached set
    // actually changed, flip `ControlPlane::plugins_restart_required` — the
    // signal Cockpit/the daemon use to prompt a restart. The production
    // `CATALOG_FEED_PUBKEY` is the all-zero placeholder, which `verify_with`
    // rejects outright (explicit guard + `verify_strict`), so the remote
    // catalog is fail-closed until a real full-order key ships; this test
    // therefore drives the manager through the `#[cfg(test)]`-only
    // `refresh_with_pubkey` seam with a throwaway key. See
    // `all_zero_placeholder_key_rejects_every_signature` for the fail-closed
    // guarantee itself.
    #[tokio::test]
    async fn manager_refresh_applies_and_marks_restart_when_changed() {
        let bytes = feed_json(9).into_bytes();
        let sig = sign(&bytes);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let settings = crate::settings::SettingsStore::new(store.clone());
        let cp = test_cp().await;
        let http: Arc<dyn CatalogHttp> = Arc::new(FakeHttp {
            feed: (200, bytes),
            sig: (200, sig),
        });

        let manager = RemoteCatalogManager::new(store.clone(), settings, cp.clone(), http);
        assert!(!cp.plugins_restart_required());

        let pubkey = test_keypair().verifying_key().to_bytes();
        let outcome = manager.refresh_with_pubkey(&pubkey).await;

        assert!(outcome.applied);
        assert_eq!(outcome.sequence, 9);
        assert_eq!(store.get_catalog_feed_sequence().await.unwrap(), 9);
        assert_eq!(store.list_remote_catalog().await.unwrap().len(), 1);
        assert!(cp.plugins_restart_required());
    }

    // A rejected (non-applied) refresh must never mark restart required, even
    // though `refresh` always calls `fetch_and_cache` — a 404/bad-sig/rollback
    // feed must leave the flag untouched.
    #[tokio::test]
    async fn manager_refresh_does_not_mark_restart_when_not_applied() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let settings = crate::settings::SettingsStore::new(store.clone());
        let cp = test_cp().await;
        let http: Arc<dyn CatalogHttp> = Arc::new(FakeHttp {
            feed: (404, vec![]),
            sig: (200, vec![]),
        });

        let manager = RemoteCatalogManager::new(store.clone(), settings, cp.clone(), http);
        let pubkey = test_keypair().verifying_key().to_bytes();
        let outcome = manager.refresh_with_pubkey(&pubkey).await;

        assert!(!outcome.applied);
        assert!(!cp.plugins_restart_required());
    }

    // Re-fetching a feed whose *content* is byte-identical to the cache must
    // NOT flip restart-required, even though `fetch_and_cache` re-stamps every
    // row with a fresh `fetched_at` (and the feed's `sequence` is accepted
    // again, since anti-rollback only rejects a strictly-lower sequence). A
    // fresh `ControlPlane` over the SAME store starts with the flag false; an
    // identical re-fetch must leave it false, or the daemon would show a
    // perpetual "restart to apply" banner every ~6h cycle.
    #[tokio::test]
    async fn manager_refresh_does_not_mark_restart_on_unchanged_refetch() {
        let bytes = feed_json(9).into_bytes();
        let sig = sign(&bytes);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let settings = crate::settings::SettingsStore::new(store.clone());
        let pubkey = test_keypair().verifying_key().to_bytes();
        let http: Arc<dyn CatalogHttp> = Arc::new(FakeHttp {
            feed: (200, bytes),
            sig: (200, sig),
        });

        // First refresh populates the cache (flag flips on the first, changed
        // fetch — asserted by the sibling test).
        let cp1 = test_cp_over(store.clone()).await;
        let mgr1 = RemoteCatalogManager::new(store.clone(), settings.clone(), cp1, http.clone());
        assert!(mgr1.refresh_with_pubkey(&pubkey).await.applied);

        // Second refresh over the SAME store with the SAME signed feed. A
        // fresh control plane's flag starts false; identical content must keep
        // it false despite the re-stamped `fetched_at`.
        let cp2 = test_cp_over(store.clone()).await;
        assert!(!cp2.plugins_restart_required());
        let mgr2 = RemoteCatalogManager::new(store.clone(), settings, cp2.clone(), http);
        let outcome = mgr2.refresh_with_pubkey(&pubkey).await;

        assert!(
            outcome.applied,
            "an identical feed still applies (re-caches)"
        );
        assert!(
            !cp2.plugins_restart_required(),
            "unchanged-content re-fetch must not flip restart-required"
        );
    }

    // A feed with one manifest-invalid entry (empty name) and one valid entry
    // must drop the invalid one, still apply, and cache only the valid id.
    #[tokio::test]
    async fn fetch_drops_invalid_entry_but_applies_valid_ones() {
        let feed = concat!(
            r#"{"schemaVersion":1,"sequence":8,"generatedAt":0,"#,
            r#""entries":["#,
            r#"{"id":"bad","manifestToml":"contract=1\nid=\"bad\"\nname=\"\"\nversion=\"1.0.0\""},"#,
            r#"{"id":"good","manifestToml":"contract=1\nid=\"good\"\nname=\"Good\"\nversion=\"2.0.0\""}"#,
            r#"],"blocked":[]}"#,
        );
        let bytes = feed.as_bytes().to_vec();
        let sig = sign(&bytes);
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let http = FakeHttp {
            feed: (200, bytes),
            sig: (200, sig),
        };
        let outcome = fetch_and_cache_with(
            &store,
            &http,
            "http://x/catalog.json",
            &test_keypair().verifying_key().to_bytes(),
        )
        .await;
        assert!(outcome.applied);
        assert_eq!(outcome.entries, 1);
        let rows = store.list_remote_catalog().await.unwrap();
        let visible: Vec<_> = rows.iter().filter(|r| !r.blocked).collect();
        assert_eq!(visible.len(), 1);
        assert_eq!(visible[0].id, "good");
    }
}
