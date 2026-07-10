//! Runtime screen commands: catalog + persisted config + real detection
//! snapshots, enriched with the latest released version from npm. Moved
//! verbatim (per the Move Recipe) from `apps/cockpit/src-tauri/src/
//! runtimes_cmd.rs`; that file keeps its own copy until the proxy rewrite in
//! Tasks 15-16. `runtime_config_status` and `reset_runtime_config` do NOT
//! move — they're pure-local home-config reads/writes that Cockpit still
//! calls directly against ryuzi-core as a library.
//!
//! `list_runtimes` is fast (reads the persisted snapshot); `refresh_runtimes`
//! re-probes binaries and asks npm, then persists the snapshot in settings.

use super::{ok, params, ApiError};
use crate::api::types::*;
use crate::control::ControlPlane;
use crate::llm_router::keys;
use crate::runtime_config::{self, ConfigStatus, EndpointInfo, RuntimeMapping};
use crate::runtimes::{self, RuntimeConfig};
use crate::serve::ApiState;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::time::Duration;

pub(crate) const HANDLES: &[&str] = &[
    "list_runtimes",
    "refresh_runtimes",
    "update_runtime_config",
    "update_runtime",
    "set_runtime_tier",
    "set_default_runtime",
    "apply_runtime_config",
];

// legacy storage keys — see Task 1 note in the plan
const SNAPSHOT_KEY: &str = "agents_snapshot";

/// Persisted probe results per agent (settings KV, JSON).
#[derive(Serialize, Deserialize, Clone, Default)]
#[serde(rename_all = "camelCase")]
struct Snapshot {
    binary_path: Option<String>,
    installed_version: Option<String>,
    latest_version: Option<String>,
    /// Locally installed models (ollama only).
    local_models: Vec<String>,
    checked_at: i64,
}

#[derive(Deserialize)]
struct UpdateRuntimeConfigP {
    id: String,
    enabled: bool,
    model: Option<String>,
    perm_mode: String,
    flags: String,
}
#[derive(Deserialize)]
struct IdP {
    id: String,
}
#[derive(Deserialize)]
struct SetRuntimeTierP {
    id: String,
    tier_id: String,
    value: Option<String>,
    combo: bool,
}
#[derive(Deserialize)]
struct ApplyRuntimeConfigP {
    id: String,
    mapping: RuntimeMappingArg,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "list_runtimes" => ok(assemble(cp).await?),
        "refresh_runtimes" => ok(refresh_runtimes(cp).await?),
        "update_runtime_config" => {
            let a: UpdateRuntimeConfigP = params(p)?;
            ok(update_runtime_config(cp, a.id, a.enabled, a.model, a.perm_mode, a.flags).await?)
        }
        "update_runtime" => {
            let a: IdP = params(p)?;
            ok(update_runtime(state, a.id).await?)
        }
        "set_runtime_tier" => {
            let a: SetRuntimeTierP = params(p)?;
            ok(set_runtime_tier(cp, a.id, a.tier_id, a.value, a.combo).await?)
        }
        "set_default_runtime" => {
            let a: IdP = params(p)?;
            ok(set_default_runtime(cp, a.id).await?)
        }
        "apply_runtime_config" => {
            let a: ApplyRuntimeConfigP = params(p)?;
            ok(apply_runtime_config(state, a.id, a.mapping).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

async fn read_snapshots(cp: &ControlPlane) -> std::collections::HashMap<String, Snapshot> {
    let raw = cp.store().get_setting(SNAPSHOT_KEY).await.ok().flatten();
    raw.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_default()
}

async fn assemble(cp: &ControlPlane) -> anyhow::Result<Vec<RuntimeInfo>> {
    let snapshots = read_snapshots(cp).await;
    let configs = runtimes::list_configs(cp.store()).await?;
    let default_agent = cp
        .store()
        .get_setting("default_agent")
        .await?
        .unwrap_or_else(|| "native".to_string());

    let mut out = Vec::new();
    for desc in runtimes::CATALOG {
        let snap = snapshots.get(desc.id).cloned().unwrap_or_default();
        let cfg = configs.iter().find(|c| c.id == desc.id);
        // The native runtime runs in-process, so it's always "available"
        // regardless of any binary on PATH.
        let is_native = desc.id == "native";
        let binary_path = if is_native {
            Some("in-process".to_string())
        } else {
            snap.binary_path.clone()
        };
        let installed_version = if is_native {
            Some(env!("CARGO_PKG_VERSION").to_string())
        } else {
            snap.installed_version.clone()
        };
        let detected = binary_path.is_some();
        let mut models: Vec<String> = desc.models.iter().map(|m| m.to_string()).collect();
        if is_native {
            // The native runtime has no fixed catalog: its selectable models
            // are the user's enabled routes + provider connections, so the
            // picker reflects what the router can actually reach today.
            models = crate::llm_router::client::selectable_native_models(cp.store()).await;
        } else if models.is_empty() {
            models = snap.local_models.clone();
        }
        let model = cfg
            .and_then(|c| c.model.clone())
            .or_else(|| models.first().cloned())
            .unwrap_or_default();
        let tiers = runtimes::list_tiers(cp.store(), desc.id)
            .await?
            .into_iter()
            .map(|t| TierInfo {
                id: t.tier_id,
                label: t.label,
                value: t.value,
                combo: t.combo,
            })
            .collect();
        out.push(RuntimeInfo {
            id: desc.id.to_string(),
            name: desc.name.to_string(),
            color: desc.color.to_string(),
            initial: desc.initial.to_string(),
            connection: desc.connection.to_string(),
            binary_path,
            installed_version,
            latest_version: snap.latest_version,
            npm_package: desc.npm_package.map(|s| s.to_string()),
            models,
            // Zero-config default: detected agents start enabled.
            enabled: cfg.map(|c| c.enabled).unwrap_or(detected),
            model,
            perm_mode: cfg
                .map(|c| c.perm_mode.clone())
                .unwrap_or_else(|| "ask".into()),
            flags: cfg.map(|c| c.flags.clone()).unwrap_or_default(),
            tiers,
            is_default: default_agent == desc.id,
            // Ryuzi-only sessions: the in-process native runtime is the only
            // session harness. Other catalog entries remain listed — they
            // still back endpoint/model configuration — but cannot run.
            runnable: is_native,
        });
    }
    Ok(out)
}

async fn npm_latest(pkg: &str) -> Option<String> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(6))
        .build()
        .ok()?;
    let url = format!("https://registry.npmjs.org/{pkg}/latest");
    let resp = client.get(url).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let v: serde_json::Value = resp.json().await.ok()?;
    Some(v.get("version")?.as_str()?.to_string())
}

/// Re-probe every catalog agent (PATH + --version + npm latest + local model
/// list for ollama), persist the snapshot, and return the fresh assembly.
async fn refresh_runtimes(cp: &ControlPlane) -> anyhow::Result<Vec<RuntimeInfo>> {
    let mut snapshots = std::collections::HashMap::new();
    for desc in runtimes::CATALOG {
        // Native runs in-process — nothing to probe on PATH; assemble marks it
        // available directly.
        if desc.id == "native" {
            continue;
        }
        let det = runtimes::detect(desc.binary).await;
        let latest = match desc.npm_package {
            Some(pkg) => npm_latest(pkg).await,
            None => None,
        };
        let local_models = match (&det.binary_path, desc.id) {
            (Some(path), "ollama") => runtimes::ollama_models(path).await,
            _ => vec![],
        };
        snapshots.insert(
            desc.id.to_string(),
            Snapshot {
                binary_path: det.binary_path,
                installed_version: det.installed_version,
                latest_version: latest,
                local_models,
                checked_at: crate::paths::now_ms(),
            },
        );
    }
    let json = serde_json::to_string(&snapshots)?;
    cp.store().set_setting(SNAPSHOT_KEY, &json).await?;
    assemble(cp).await
}

async fn update_runtime_config(
    cp: &ControlPlane,
    id: String,
    enabled: bool,
    model: Option<String>,
    perm_mode: String,
    flags: String,
) -> anyhow::Result<Vec<RuntimeInfo>> {
    runtimes::upsert_config(
        cp.store(),
        RuntimeConfig {
            id,
            enabled,
            model,
            perm_mode,
            flags,
        },
    )
    .await?;
    assemble(cp).await
}

async fn set_runtime_tier(
    cp: &ControlPlane,
    id: String,
    tier_id: String,
    value: Option<String>,
    combo: bool,
) -> anyhow::Result<Vec<RuntimeInfo>> {
    runtimes::set_tier(cp.store(), &id, &tier_id, value, combo).await?;
    assemble(cp).await
}

async fn set_default_runtime(cp: &ControlPlane, id: String) -> anyhow::Result<Vec<RuntimeInfo>> {
    cp.store().set_setting("default_agent", &id).await?;
    assemble(cp).await
}

/// Fire-and-forget npm update; progress streams via CoreEvent
/// RuntimeUpdateLog / RuntimeUpdateDone, then a refreshed snapshot matters —
/// the UI calls refreshRuntimes() on Done.
async fn update_runtime(state: &ApiState, id: String) -> Result<(), ApiError> {
    let cp = &state.cp;
    let desc = runtimes::descriptor(&id)
        .ok_or_else(|| ApiError::not_found(format!("unknown runtime: {id}")))?;
    let Some(pkg) = desc.npm_package else {
        return Err(ApiError::bad_request(format!(
            "{} is not npm-managed",
            desc.name
        )));
    };
    let events = cp.events_sender();
    let pkg = pkg.to_string();
    tokio::spawn(async move {
        let res = runtimes::run_npm_update(events.clone(), &id, &pkg).await;
        let (ok, message) = match res {
            Ok(true) => (true, None),
            Ok(false) => (false, Some("npm exited with a non-zero status".to_string())),
            Err(e) => (false, Some(e.to_string())),
        };
        let _ = events.send(crate::domain::CoreEvent::RuntimeUpdateDone {
            runtime_id: id,
            ok,
            message,
        });
    });
    Ok(())
}

// ---------------------------------------------------------------------------
// Endpoint config apply (spec §5) — write the local router's base URL + key
// into native CLI-tool configs (claude/codex/opencode only). Reset and status
// stay Tauri-only (pure-local home-config); this only needs the shared
// `status_of`/`home` helpers to report the post-apply status.
// ---------------------------------------------------------------------------

fn home() -> Result<std::path::PathBuf, ApiError> {
    dirs::home_dir().ok_or_else(|| ApiError::bad_request("cannot resolve home directory"))
}

fn status_of(id: &str, home: &std::path::Path) -> Result<RuntimeConfigStatusInfo, ApiError> {
    let st: Option<ConfigStatus> = match id {
        "claude" => Some(runtime_config::claude_status(home)?),
        "codex" => Some(runtime_config::codex_status(home)?),
        "opencode" => Some(runtime_config::opencode_status(home)?),
        _ => None,
    };
    Ok(match st {
        Some(s) => RuntimeConfigStatusInfo {
            config_path: s.config_path,
            exists: s.exists,
            configured: s.configured,
            supported: true,
        },
        None => RuntimeConfigStatusInfo {
            config_path: String::new(),
            exists: false,
            configured: false,
            supported: false,
        },
    })
}

/// Guard (spec §5): refuse to write configs that point at a dead endpoint —
/// the server must be running and at least one endpoint key must exist.
async fn apply_runtime_config(
    state: &ApiState,
    id: String,
    mapping: RuntimeMappingArg,
) -> Result<RuntimeConfigStatusInfo, ApiError> {
    let cp = &state.cp;
    let srv = &state.router_server;
    let st = srv.status();
    if !st.running {
        return Err(ApiError::bad_request(
            "The endpoint server is not running. Start it in Models → Endpoint first.",
        ));
    }
    let key = keys::first_key(cp.store()).await?.ok_or_else(|| {
        ApiError::bad_request("No endpoint API key exists. Create one in Models → Endpoint first.")
    })?;
    if mapping.model.trim().is_empty() {
        return Err(ApiError::bad_request(
            "No model selected. Add an enabled provider connection in Models → Providers first.",
        ));
    }
    let ep = EndpointInfo {
        base_url: format!("http://127.0.0.1:{}", st.port),
        api_key: key.key,
    };
    let m = RuntimeMapping {
        model: mapping.model,
        opus: mapping.opus,
        sonnet: mapping.sonnet,
        haiku: mapping.haiku,
        models: mapping.models,
    };
    let home = home()?;
    match id.as_str() {
        "claude" => runtime_config::claude_apply(&home, &ep, &m)?,
        "codex" => runtime_config::codex_apply(&home, &ep, &m)?,
        "opencode" => runtime_config::opencode_apply(&home, &ep, &m)?,
        other => {
            return Err(ApiError::bad_request(format!(
                "config apply is not supported for '{other}' yet"
            )))
        }
    }
    status_of(&id, &home)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;

    #[tokio::test]
    async fn native_runtime_is_listed_and_available() {
        let s = state().await;
        let list = assemble(&s.cp).await.unwrap();
        let native = list
            .iter()
            .find(|r| r.id == "native")
            .expect("native runtime must appear in the Runtime list");
        assert_eq!(native.name, "Ryuzi");
        // Available (in-process) without any binary on PATH, and runnable.
        assert!(native.binary_path.is_some(), "native must be available");
        assert!(native.installed_version.is_some());
        assert!(native.runnable, "native must be runnable");
        assert!(
            native.is_default,
            "native is the default agent when none is set"
        );
        // Ryuzi-only sessions: nothing else can run, but the other catalog
        // runtimes stay listed for endpoint/model configuration.
        assert!(list.len() > 1, "other runtimes must remain listed");
        assert!(
            list.iter()
                .filter(|r| r.id != "native")
                .all(|r| !r.runnable),
            "only native is runnable"
        );
    }

    #[tokio::test]
    async fn list_runtimes_dispatches_via_rpc() {
        let s = state().await;
        let out = dispatch(&s, "list_runtimes", json!({})).await.unwrap();
        assert!(out.as_array().unwrap().iter().any(|r| r["id"] == "native"));
    }

    #[tokio::test]
    async fn set_default_runtime_round_trip_via_rpc() {
        let s = state().await;
        let out = dispatch(&s, "set_default_runtime", json!({"id": "native"}))
            .await
            .unwrap();
        let native = out
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "native")
            .unwrap();
        assert_eq!(native["isDefault"], true);
    }

    #[tokio::test]
    async fn apply_runtime_config_requires_a_running_endpoint_server() {
        let s = state().await;
        let err = dispatch(
            &s,
            "apply_runtime_config",
            json!({"id": "claude", "mapping": {
                "model": "m1", "opus": null, "sonnet": null, "haiku": null, "models": []
            }}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.status, 400);
        assert!(err.message.contains("endpoint server is not running"));
    }
}
