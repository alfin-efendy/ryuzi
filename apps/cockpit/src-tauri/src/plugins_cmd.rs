//! Plugins screen commands: thin proxies to the engine daemon's plugins RPC
//! family — every installed plugin's identity/capabilities (`list_plugins`),
//! a single plugin's full detail (`plugin_detail`), enable/disable
//! (`set_plugin_enabled`), a validated settings write
//! (`set_plugin_setting`), plugin OAuth sign-in, a provider's effective
//! model list (`plugin_models`), kind-symmetric `uninstall_plugin`, the
//! install wizard (`begin_plugin_install` / `set_plugin_oauth_client_id` /
//! `cancel_plugin_install`), the skill/plugin distribution surface
//! (`begin_skill_install` / `confirm_skill_install` / `update_plugin` /
//! `update_all_plugins` / `set_plugin_pin` / `plugin_doctor` /
//! `plugins_restart_required`), and the remote catalog surface
//! (`refresh_catalog` / `catalog_status`).
//!
//! Behavior change from the pre-daemon version: `begin_plugin_oauth` and
//! `begin_plugin_install` no longer open the system browser directly — the
//! engine broadcasts `CoreEvent::PluginOauthAuthorizeUrl` over SSE, and the
//! bridge in `lib.rs` opens the browser (and re-emits the legacy Tauri
//! event) on receipt.
//!
//! Cockpit-local: the install wizard's loopback OAuth callback server. The
//! DCR-registered `redirect_uri` points at `127.0.0.1:8976` on the USER's
//! machine (the daemon may be remote), so the callback capture is inherently
//! local. The daemon owns discovery / DCR / token exchange / PKCE flow
//! state; Cockpit only binds the port, awaits the callback, validates
//! `state` locally, and hands the code back via `complete_plugin_oauth`.

use crate::engine::EngineClient;
use crate::error::CmdError;
use crate::events::PluginOauthCompletedMsg;
use ryuzi_core::oauth_loopback;
use ryuzi_core::skills_install::InstalledSkillPack;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tauri::{AppHandle, State};
use tauri_specta::Event as _;
use tokio::sync::oneshot;

// `PluginFieldInfo`/`PluginMcpInfo`/`TrustPromptDto` are only reachable
// transitively (as fields of `PluginDetail`/`SkillInstallBegin`/
// `UpdateOutcomeDto`) but are re-exported by name anyway for a complete,
// documented DTO surface; specta still emits them via the type graph either
// way.
#[allow(unused_imports)]
pub use ryuzi_core::api::types::{
    CatalogStatus, DoctorFinding, PluginAuthInfo, PluginDetail, PluginFieldInfo, PluginInfo,
    PluginInstallBeginResult, PluginMcpInfo, PluginOauthBeginResult, SkillInstallBegin,
    TrustPromptDto, UpdateOutcomeDto, UpdateOutcomeEntry,
};

type R<T> = Result<T, CmdError>;
type Engine<'a> = State<'a, Arc<EngineClient>>;

#[tauri::command]
#[specta::specta]
pub async fn list_plugins(engine: Engine<'_>) -> R<Vec<PluginInfo>> {
    engine.rpc("list_plugins", serde_json::json!({})).await
}

#[tauri::command]
#[specta::specta]
pub async fn plugin_detail(engine: Engine<'_>, id: String) -> R<PluginDetail> {
    engine
        .rpc("plugin_detail", serde_json::json!({ "id": id }))
        .await
}

/// Same semantics as `ryuzi plugins enable/disable` — delegates to the
/// shared core helper so the two surfaces never drift.
#[tauri::command]
#[specta::specta]
pub async fn set_plugin_enabled(engine: Engine<'_>, id: String, enabled: bool) -> R<()> {
    engine
        .rpc(
            "set_plugin_enabled",
            serde_json::json!({ "id": id, "enabled": enabled }),
        )
        .await
}

/// Validated write through `SettingsStore::set` — rejects unknown keys and
/// type-mismatched values the same way `ryuzi config set` does. Never
/// returns a value, so no secret can leak back through this command.
#[tauri::command]
#[specta::specta]
pub async fn set_plugin_setting(engine: Engine<'_>, key: String, value: String) -> R<()> {
    engine
        .rpc(
            "set_plugin_setting",
            serde_json::json!({ "key": key, "value": value }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn begin_plugin_oauth(
    engine: Engine<'_>,
    plugin_id: String,
) -> R<PluginOauthBeginResult> {
    engine
        .rpc(
            "begin_plugin_oauth",
            serde_json::json!({ "plugin_id": plugin_id }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn complete_plugin_oauth(
    engine: Engine<'_>,
    plugin_id: String,
    code: String,
    state_token: String,
) -> R<PluginAuthInfo> {
    engine
        .rpc(
            "complete_plugin_oauth",
            serde_json::json!({ "plugin_id": plugin_id, "code": code, "state_token": state_token }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn disconnect_plugin_oauth(engine: Engine<'_>, plugin_id: String) -> R<PluginAuthInfo> {
    engine
        .rpc(
            "disconnect_plugin_oauth",
            serde_json::json!({ "plugin_id": plugin_id }),
        )
        .await
}

#[tauri::command]
#[specta::specta]
pub async fn plugin_models(engine: Engine<'_>, id: String) -> R<Vec<String>> {
    engine
        .rpc("plugin_models", serde_json::json!({ "id": id }))
        .await
}

/// The wizard's loopback callback server port. Registered redirect URIs use
/// it, so it can never change without re-registering every DCR client. Must
/// stay equal to the daemon's `PLUGIN_OAUTH_CALLBACK_PORT` — the daemon
/// builds the `redirect_uri` that DCR registers against this exact port.
const PLUGIN_OAUTH_CALLBACK_PORT: u16 = 8976;

fn plugin_oauth_flow_key(plugin_id: &str, state_token: &str) -> String {
    format!("{plugin_id}:{state_token}")
}

fn plugin_oauth_callback_path(plugin_id: &str) -> String {
    format!("/plugin-oauth/{plugin_id}/callback")
}

/// Cancellation handles for pending local loopback callback servers, keyed by
/// `{plugin_id}:{state_token}`. Firing (or dropping) one makes the background
/// task exit without emitting a completion event. This is the Cockpit-local
/// half of the install-cancel path; the daemon owns the PKCE flow state
/// (verifier / redirect / scopes).
static PLUGIN_INSTALL_CANCELS: OnceLock<Mutex<HashMap<String, oneshot::Sender<()>>>> =
    OnceLock::new();

fn plugin_install_cancels() -> &'static Mutex<HashMap<String, oneshot::Sender<()>>> {
    PLUGIN_INSTALL_CANCELS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Shut down any live local callback server(s) for `plugin_id` — all of its
/// flows when `state_token` is `None`, else just the one keyed by
/// `{plugin_id}:{state_token}`. Fired by the local half of
/// `cancel_plugin_install` and by a same-plugin re-begin (Retry).
fn cancel_pending_local_flows(plugin_id: &str, state_token: Option<&str>) {
    let prefix = format!("{plugin_id}:");
    let mut cancels = plugin_install_cancels()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner());
    let keys: Vec<String> = match state_token {
        Some(token) => vec![plugin_oauth_flow_key(plugin_id, token)],
        None => cancels
            .keys()
            .filter(|k| k.starts_with(&prefix))
            .cloned()
            .collect(),
    };
    for key in keys {
        if let Some(tx) = cancels.remove(&key) {
            let _ = tx.send(());
        }
    }
}

/// Kind-symmetric uninstall on the daemon; returns the refreshed list so the
/// entry's `installed` flips false and it reappears in Browse.
#[tauri::command]
#[specta::specta]
pub async fn uninstall_plugin(engine: Engine<'_>, id: String) -> R<Vec<PluginInfo>> {
    engine
        .rpc("uninstall_plugin", serde_json::json!({ "id": id }))
        .await
}

/// Persist a manually-entered OAuth client id (external-OAuth plugins store
/// it under the declared `auth.setting`; everyone else in
/// `plugin_oauth_clients`). Pure daemon write.
#[tauri::command]
#[specta::specta]
pub async fn set_plugin_oauth_client_id(
    engine: Engine<'_>,
    plugin_id: String,
    client_id: String,
) -> R<()> {
    engine
        .rpc(
            "set_plugin_oauth_client_id",
            serde_json::json!({ "plugin_id": plugin_id, "client_id": client_id }),
        )
        .await
}

/// The install wizard's entry point. The daemon (steps 1-6) resolves the auth
/// kind, discovers / registers OAuth endpoints, builds the authorize URL,
/// stores the PKCE flow state, and emits `CoreEvent::PluginOauthAuthorizeUrl`
/// (the lib.rs SSE bridge opens the browser). When an OAuth flow was prepared,
/// Cockpit adds step 7 here: it binds the fixed local callback port `8976`
/// (retried briefly — a same-plugin Retry's previous axum server shuts down
/// asynchronously), spawns a background task that awaits the loopback callback
/// (or a cancel), validates `state` locally, and hands the code back to the
/// daemon via `complete_plugin_oauth` for exchange + token storage. Degrades
/// to `callback_mode: "manual"` (paste) only when the port stays taken.
#[tauri::command]
#[specta::specta]
pub async fn begin_plugin_install(
    app: AppHandle,
    engine: Engine<'_>,
    plugin_id: String,
) -> R<PluginInstallBeginResult> {
    let mut result: PluginInstallBeginResult = engine
        .rpc(
            "begin_plugin_install",
            serde_json::json!({ "plugin_id": plugin_id }),
        )
        .await?;
    let Some(begin) = result.oauth_begin.clone() else {
        return Ok(result);
    };

    // A same-plugin re-begin (Retry) must shut down the previous flow's local
    // callback server before we try to bind the fixed port again. The daemon
    // already dropped its own stale flow state in `begin_plugin_install`.
    cancel_pending_local_flows(&plugin_id, None);

    // 7. Register a cancel handle for this flow BEFORE the bind-retry loop:
    // cancel_plugin_install can arrive while that loop is still running, and
    // if the sender isn't in the map yet the local cancel finds nothing to
    // signal — the eventually-spawned task would then hold the port for the
    // full 5-minute timeout. Registering first means a cancel that fires
    // during the bind window pre-fires cancel_rx, and the spawned task's
    // `tokio::select!` resolves on its first poll.
    let (cancel_tx, cancel_rx) = oneshot::channel::<()>();
    let flow_key = plugin_oauth_flow_key(&plugin_id, &begin.state_token);
    plugin_install_cancels()
        .lock()
        .unwrap_or_else(|poison| poison.into_inner())
        .insert(flow_key.clone(), cancel_tx);

    // Callback server on the fixed wizard port. The just-canceled previous
    // flow's axum server shuts down asynchronously — the port can still be
    // held for a moment. Retry the bind briefly (3 attempts, 100ms apart)
    // before concluding it is genuinely taken by another plugin's flow.
    let mut bound = oauth_loopback::bind_fixed(PLUGIN_OAUTH_CALLBACK_PORT).await;
    for _ in 0..2 {
        if bound.is_ok() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        bound = oauth_loopback::bind_fixed(PLUGIN_OAUTH_CALLBACK_PORT).await;
    }
    match bound {
        Ok(listener) => {
            result.callback_mode = "auto".to_string();
            let (server, result_rx, shutdown_tx) = oauth_loopback::spawn_callback_server(
                listener,
                &plugin_oauth_callback_path(&plugin_id),
            );
            let engine_client = engine.inner().clone();
            let app_handle = app.clone();
            let task_plugin_id = plugin_id.clone();
            let state_token = begin.state_token.clone();
            tauri::async_runtime::spawn(async move {
                let outcome = tokio::select! {
                    res = oauth_loopback::await_callback(
                        server,
                        result_rx,
                        shutdown_tx,
                        std::time::Duration::from_secs(5 * 60),
                    ) => Some(res),
                    // Cancellation (wizard closed / re-begin): dropping the
                    // await_callback future drops shutdown_tx, which shuts
                    // the axum server down gracefully.
                    _ = cancel_rx => None,
                };
                plugin_install_cancels()
                    .lock()
                    .unwrap_or_else(|poison| poison.into_inner())
                    .remove(&flow_key);
                // Canceled: exit silently — the wizard initiated it.
                let Some(res) = outcome else { return };
                let msg = match res {
                    Ok(callback) => {
                        complete_local_callback(
                            &engine_client,
                            &task_plugin_id,
                            &state_token,
                            callback,
                        )
                        .await
                    }
                    Err(err) => PluginOauthCompletedMsg {
                        plugin_id: task_plugin_id.clone(),
                        ok: false,
                        error: Some(err.to_string()),
                    },
                };
                let _ = msg.emit(&app_handle);
            });
        }
        Err(_) => {
            // Port still taken after the retries — another plugin's flow is
            // pending. No background task will consume cancel_rx here, so
            // remove the sender registered above to avoid leaking it. The
            // wizard explains why; completion goes through
            // complete_plugin_oauth (manual paste).
            plugin_install_cancels()
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .remove(&flow_key);
            result.callback_mode = "manual".to_string();
        }
    }

    // The daemon already emitted CoreEvent::PluginOauthAuthorizeUrl (the
    // lib.rs SSE bridge opens the browser) — do NOT open it again here.
    Ok(result)
}

/// Validate a captured loopback callback's `state` LOCALLY (a mismatch is
/// discarded WITHOUT touching the daemon flow, so the flow entry survives for
/// manual paste), then hand the code to the daemon's `complete_plugin_oauth`
/// for exchange + token storage.
async fn complete_local_callback(
    engine: &EngineClient,
    plugin_id: &str,
    state_token: &str,
    callback: oauth_loopback::CallbackResult,
) -> PluginOauthCompletedMsg {
    let completed = |ok: bool, error: Option<String>| PluginOauthCompletedMsg {
        plugin_id: plugin_id.to_string(),
        ok,
        error,
    };
    let Some(code) = callback.code else {
        return completed(
            false,
            Some("OAuth callback did not include a `code` parameter".to_string()),
        );
    };
    let Some(state) = callback.state else {
        return completed(
            false,
            Some("OAuth callback did not include a `state` parameter".to_string()),
        );
    };
    if state != state_token {
        return completed(
            false,
            Some(
                "OAuth state mismatch — the sign-in response did not match this install"
                    .to_string(),
            ),
        );
    }
    match engine
        .rpc::<PluginAuthInfo>(
            "complete_plugin_oauth",
            serde_json::json!({
                "plugin_id": plugin_id,
                "code": code.trim(),
                "state_token": state_token,
            }),
        )
        .await
    {
        Ok(_) => completed(true, None),
        Err(err) => completed(false, Some(err.message)),
    }
}

/// Cancel a pending install: fire the LOCAL loopback shutdown first (frees
/// port 8976 immediately), then drop the daemon's flow state.
#[tauri::command]
#[specta::specta]
pub async fn cancel_plugin_install(
    engine: Engine<'_>,
    plugin_id: String,
    state_token: Option<String>,
) -> R<()> {
    cancel_pending_local_flows(&plugin_id, state_token.as_deref());
    engine
        .rpc(
            "cancel_plugin_install",
            serde_json::json!({ "plugin_id": plugin_id, "state_token": state_token }),
        )
        .await
}

// ---------- Skill/plugin distribution: trust prompt, update, pin, doctor ----------
//
// All thin proxies to the daemon's plugins RPC family. The daemon owns the
// two-phase tiered trust gate, the deterministic update/pin logic, the ledger
// writes, and the `plugins_restart_required` latch (it is the host, so it is
// the only process whose in-memory plugin set a restart would refresh). The
// mirror DTOs (`SkillInstallBegin`/`UpdateOutcomeDto`/`UpdateOutcomeEntry`/
// `DoctorFinding`/`TrustPromptDto`) live in `ryuzi_core::api::types`.

/// Phase 1 of the two-phase tiered trust gate: curated sources install
/// immediately (`completed: true`); arbitrary sources stop at a
/// `TrustPromptDto` the wizard must show before `confirm_skill_install` can
/// proceed.
#[tauri::command]
#[specta::specta]
pub async fn begin_skill_install(engine: Engine<'_>, source: String) -> R<SkillInstallBegin> {
    engine
        .rpc(
            "begin_skill_install",
            serde_json::json!({ "source": source }),
        )
        .await
}

/// Phase 2: complete a staged install (or update) after the user has
/// acknowledged its `TrustPromptDto`. The token is single-use.
#[tauri::command]
#[specta::specta]
pub async fn confirm_skill_install(engine: Engine<'_>, token: String) -> R<InstalledSkillPack> {
    engine
        .rpc(
            "confirm_skill_install",
            serde_json::json!({ "token": token }),
        )
        .await
}

/// Update one installed pack. `force` overrides the local-edits guard but
/// never the pinned guard or the hook-script re-ack gate.
#[tauri::command]
#[specta::specta]
pub async fn update_plugin(engine: Engine<'_>, id: String, force: bool) -> R<UpdateOutcomeDto> {
    engine
        .rpc(
            "update_plugin",
            serde_json::json!({ "id": id, "force": force }),
        )
        .await
}

/// Update every installed pack (skipping pinned ones); never fails as a whole
/// — a single pack's error surfaces as that pack's `UpdateOutcomeDto::Failed`
/// entry.
#[tauri::command]
#[specta::specta]
pub async fn update_all_plugins(engine: Engine<'_>) -> R<Vec<UpdateOutcomeEntry>> {
    engine
        .rpc("update_all_plugins", serde_json::json!({}))
        .await
}

/// Pin (or unpin) an installed pack against future updates.
#[tauri::command]
#[specta::specta]
pub async fn set_plugin_pin(
    engine: Engine<'_>,
    id: String,
    pinned: bool,
    reason: Option<String>,
) -> R<()> {
    engine
        .rpc(
            "set_plugin_pin",
            serde_json::json!({ "id": id, "pinned": pinned, "reason": reason }),
        )
        .await
}

/// Read-only plugin health aggregation — see the daemon's
/// `plugins::doctor::plugin_doctor` for the full list of checks. Never mutates
/// state.
#[tauri::command]
#[specta::specta]
pub async fn plugin_doctor(engine: Engine<'_>) -> R<Vec<DoctorFinding>> {
    engine.rpc("plugin_doctor", serde_json::json!({})).await
}

/// Whether a plugin install/update since the daemon's last start requires a
/// restart to take effect (in-memory flag on the daemon's `ControlPlane`,
/// cleared only by a daemon restart).
#[tauri::command]
#[specta::specta]
pub async fn plugins_restart_required(engine: Engine<'_>) -> R<bool> {
    engine
        .rpc("plugins_restart_required", serde_json::json!({}))
        .await
}

// ---------- Remote catalog: force-refresh + status ----------

/// Force an out-of-cadence catalog fetch right now — `RemoteCatalogManager`'s
/// background timer lives daemon-side and is not otherwise user-triggerable.
/// Returns the same snapshot shape as `catalog_status`.
#[tauri::command]
#[specta::specta]
pub async fn refresh_catalog(engine: Engine<'_>) -> R<CatalogStatus> {
    engine.rpc("refresh_catalog", serde_json::json!({})).await
}

/// Last accepted feed's sequence/outcome plus cached entry/blocked counts.
/// Read-only, never mutates state.
#[tauri::command]
#[specta::specta]
pub async fn catalog_status(engine: Engine<'_>) -> R<CatalogStatus> {
    engine.rpc("catalog_status", serde_json::json!({})).await
}
