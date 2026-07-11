//! Per-runner engine client registry. Cockpit talks to zero-or-more engine
//! daemons: always `"local"` (attached-to or spawned via
//! [`crate::engine::connect_or_spawn`]), plus zero or more paired remote
//! runners — pinned-TLS clients (`EngineClient::new_pinned`) built from rows
//! `save_runner` persisted in the LOCAL engine's store (P3-1/P3-2).
//!
//! ## Security invariant: the decrypted device token never reaches the webview
//!
//! The remote runner rows live in the local engine's `gateways` table with
//! `device_token` value-encrypted (`llm_router::secrets::encrypt_field`).
//! Building a pinned `EngineClient` needs the plaintext bearer token, so
//! [`EngineManager::load_remotes`] calls a backend-only RPC method on the
//! LOCAL engine, `list_runner_credentials` (see
//! `crates/core/src/api/gateways_api.rs`), which decrypts and returns it.
//!
//! That method is reachable ONLY the way any RPC method is reachable: a
//! bearer-token-gated HTTP call to the local daemon's loopback control API.
//! It is deliberately never wrapped in a `#[tauri::command]` and never
//! listed in `lib.rs`'s `collect_commands!` — the webview's `invoke()` can
//! only call functions registered there, so it has no path to this method
//! or its result. The only caller in this codebase is `load_remotes` below,
//! and the only thing it does with the decrypted token is hand it to
//! `EngineClient::new_pinned`, which stores it as a `reqwest::Client`
//! bearer-auth header used solely for outbound calls to that runner. It is
//! never placed on a `CoreEventMsg`/other emitted Tauri event, never
//! returned by a command, and never logged.
//!
//! Do not add a `#[tauri::command]` (or any other JS-reachable path) that
//! proxies `list_runner_credentials` or otherwise forwards a decrypted
//! `device_token` to the frontend.

use crate::engine::EngineClient;
use crate::error::CmdError;
use crate::events;
use std::collections::HashMap;
use std::sync::{Arc, RwLock};
use tauri::AppHandle;
use tauri_specta::Event as _;

/// Wire shape of the LOCAL engine's backend-only `list_runner_credentials`
/// RPC result: a paired remote runner row with `device_token` decrypted.
/// Deserialize-only — this type is never serialized back out, so it can
/// never accidentally end up behind a `#[specta::specta]` command return.
#[derive(Debug, Clone, serde::Deserialize)]
struct RunnerCredential {
    id: String,
    #[allow(dead_code)] // carried for future use (P3-6 live-add naming); unused today.
    name: String,
    host: String,
    port: u16,
    fingerprint: String,
    device_token: String,
}

/// Holds one [`EngineClient`] per runner id (`"local"` plus zero or more
/// paired remote runners) and the SSE bridge task handle for each, so
/// runners can be added (P3-6's live-add) or, later, removed at runtime.
pub struct EngineManager {
    clients: RwLock<HashMap<String, Arc<EngineClient>>>,
    bridges: RwLock<HashMap<String, tauri::async_runtime::JoinHandle<()>>>,
}

impl EngineManager {
    /// Seeds only `"local"`, via [`crate::engine::connect_or_spawn`]. Call
    /// [`EngineManager::start_bridge`] for `"local"` and
    /// [`EngineManager::load_remotes`] afterwards to bring up the SSE
    /// bridges and populate paired remote runners.
    pub async fn bootstrap_local() -> anyhow::Result<(Self, Arc<EngineClient>)> {
        let client = Arc::new(crate::engine::connect_or_spawn().await?);
        let mut clients = HashMap::new();
        clients.insert("local".to_string(), client.clone());
        let manager = EngineManager {
            clients: RwLock::new(clients),
            bridges: RwLock::new(HashMap::new()),
        };
        Ok((manager, client))
    }

    /// Look up the client for `runner_id`. Returns a clear "unknown runner"
    /// error rather than panicking — callers (Tauri commands, once P3-4
    /// threads `runner_id` through them) surface this as a normal `CmdError`
    /// to the frontend.
    pub fn client(&self, runner_id: &str) -> Result<Arc<EngineClient>, CmdError> {
        self.clients
            .read()
            .expect("EngineManager::clients lock poisoned")
            .get(runner_id)
            .cloned()
            .ok_or_else(|| CmdError {
                message: format!("unknown runner: {runner_id}"),
            })
    }

    /// Spawn (and record the handle for) `runner_id`'s SSE bridge.
    pub fn start_bridge(
        &self,
        runner_id: String,
        client: Arc<EngineClient>,
        app_handle: &AppHandle,
    ) {
        let handle = spawn_bridge(runner_id.clone(), client, app_handle.clone());
        self.bridges
            .write()
            .expect("EngineManager::bridges lock poisoned")
            .insert(runner_id, handle);
    }

    /// Live-add a single just-paired runner (P3-6's "Add Runner" flow):
    /// build its pinned `EngineClient` and spawn its SSE bridge, exactly
    /// like one iteration of [`EngineManager::load_remotes`]'s loop, so a
    /// freshly paired runner is usable immediately — no Cockpit restart
    /// required to pick it up. The row itself must already be persisted (by
    /// the LOCAL engine's `save_runner` RPC) before this is called; this
    /// method only updates in-memory state. `device_token` is plaintext —
    /// same security posture as `load_remotes`: it's used solely to build
    /// the pinned client's bearer header and is never logged, stored here,
    /// or handed back to a caller.
    pub fn add_runner(
        &self,
        runner_id: String,
        host: String,
        port: u16,
        device_token: String,
        fingerprint: String,
        app_handle: &AppHandle,
    ) {
        let client = Arc::new(EngineClient::new_pinned(
            format!("https://{host}:{port}"),
            device_token,
            fingerprint,
        ));
        self.clients
            .write()
            .expect("EngineManager::clients lock poisoned")
            .insert(runner_id.clone(), client.clone());
        self.start_bridge(runner_id, client, app_handle);
    }

    /// (Re)load paired remote runners: ask the LOCAL engine's backend-only
    /// `list_runner_credentials` RPC for every `remote`-kind row with its
    /// `device_token` decrypted (see module docs for why this never reaches
    /// JS), build a pinned `EngineClient` per row, insert it under the
    /// runner id, and start its SSE bridge.
    pub async fn load_remotes(&self, app_handle: &AppHandle) -> anyhow::Result<()> {
        let local = self
            .client("local")
            .map_err(|e| anyhow::anyhow!(e.message))?;
        let rows: Vec<RunnerCredential> = local
            .rpc("list_runner_credentials", serde_json::json!({}))
            .await
            .map_err(|e| anyhow::anyhow!(e.message))?;
        for row in rows {
            let client = Arc::new(EngineClient::new_pinned(
                format!("https://{}:{}", row.host, row.port),
                row.device_token,
                row.fingerprint,
            ));
            self.clients
                .write()
                .expect("EngineManager::clients lock poisoned")
                .insert(row.id.clone(), client.clone());
            self.start_bridge(row.id, client, app_handle);
        }
        Ok(())
    }
}

/// Forward every event off `client`'s SSE stream to the webview, stamping
/// `runner_id` on every emitted `CoreEventMsg`/`OauthAuthorizeUrlMsg`/
/// `PluginOauthAuthorizeUrlMsg`. Reconnects with exponential backoff (500ms
/// -> 30s cap) whenever the stream ends or errors — the daemon behind
/// `runner_id` may restart independently of Cockpit (and, for a remote
/// runner, may simply be offline for a while). OAuth-authorize-URL events
/// are additionally mapped onto their legacy Tauri events AND trigger a
/// local browser open (no daemon has a webview to open one from).
///
/// This is the single implementation behind every runner's bridge — the
/// `"local"` runner uses it exactly like every remote runner does (ported
/// verbatim, parameterized by `runner_id`, from the pre-multi-runner single
/// bridge task that used to live inline in `lib.rs`'s `setup()`).
pub fn spawn_bridge(
    runner_id: String,
    client: Arc<EngineClient>,
    app_handle: AppHandle,
) -> tauri::async_runtime::JoinHandle<()> {
    tauri::async_runtime::spawn(async move {
        use futures::StreamExt;
        let mut backoff_ms: u64 = 500;
        loop {
            match client.events().await {
                Ok(stream) => {
                    backoff_ms = 500;
                    let mut stream = Box::pin(stream);
                    while let Some(v) = stream.next().await {
                        match v.get("kind").and_then(|k| k.as_str()) {
                            Some("oauthAuthorizeUrl") => {
                                let provider = v["provider"].as_str().unwrap_or("").to_string();
                                let url = v["authorize_url"].as_str().unwrap_or("").to_string();
                                let _ = tauri_plugin_opener::open_url(url.clone(), None::<String>);
                                let _ = events::OauthAuthorizeUrlMsg {
                                    runner_id: runner_id.clone(),
                                    provider,
                                    authorize_url: url,
                                }
                                .emit(&app_handle);
                            }
                            Some("pluginOauthAuthorizeUrl") => {
                                let plugin_id = v["plugin_id"].as_str().unwrap_or("").to_string();
                                let url = v["authorize_url"].as_str().unwrap_or("").to_string();
                                let _ = tauri_plugin_opener::open_url(url.clone(), None::<String>);
                                let _ = events::PluginOauthAuthorizeUrlMsg {
                                    runner_id: runner_id.clone(),
                                    plugin_id,
                                    authorize_url: url,
                                }
                                .emit(&app_handle);
                            }
                            _ => {
                                if let Ok(ev) = serde_json::from_value::<ryuzi_core::CoreEvent>(v) {
                                    let _ = events::CoreEventMsg {
                                        runner_id: runner_id.clone(),
                                        event: ev,
                                    }
                                    .emit(&app_handle);
                                }
                            }
                        }
                    }
                    eprintln!(
                        "[ryuzi] engine event stream ended (runner {runner_id}) — reconnecting"
                    );
                }
                Err(e) => eprintln!(
                    "[ryuzi] engine event stream error (runner {runner_id}): {} — retrying",
                    e.message
                ),
            }
            tokio::time::sleep(std::time::Duration::from_millis(backoff_ms)).await;
            backoff_ms = (backoff_ms * 2).min(30_000);
        }
    })
}
