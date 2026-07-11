//! `extension_status` rpc (Track D observability — DT8): a read-only,
//! params-free snapshot of every extension (code plugin) the daemon's
//! `ExtensionHost` currently knows about, mirroring
//! `remote_catalog_api::catalog_status`'s own params-free-status-snapshot
//! shape and `plugins::doctor::plugin_doctor`'s "only report on host state
//! that's actually there" emptiness discipline (see both for the pattern
//! this reuses).
//!
//! Never mutates anything: no spawn, no restart, no shutdown. Cockpit's
//! `PluginDetailView` calls this (via the `extension_status` Tauri thin
//! command) to render an extension-capable plugin's live state, restart
//! count, and sanitized last error.

use super::{ok, ApiError};
use crate::api::types::ExtensionStatusEntry;
use crate::control::ControlPlane;
use crate::plugins::extension::ExtensionStatus;
use crate::serve::ApiState;
use crate::settings::SettingsStore;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &["extension_status"];

pub(crate) async fn dispatch(state: &ApiState, method: &str, _p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "extension_status" => ok(extension_status(cp).await?),
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

/// One entry per spawned extension, across every enabled extension-capable
/// plugin, plus a synthetic `not-running` entry for an enabled
/// extension-capable plugin the host has no spawned entry for at all — same
/// enumeration `plugins::doctor::plugin_doctor` uses (see its "Extension...
/// health" section), just projected into the full status DTO instead of only
/// the unhealthy branches. Gated on `ExtensionHost::is_empty()` the same way:
/// a control plane that never spawned anything (every test `ControlPlane`,
/// or a process that isn't the daemon's spawn host) reports an empty list
/// rather than a `not-running` entry per enabled extension plugin — an
/// unspawned host is not evidence any specific extension failed.
async fn extension_status(cp: &ControlPlane) -> anyhow::Result<Vec<ExtensionStatusEntry>> {
    let mut out = Vec::new();
    if cp.extension_host().is_empty().await {
        return Ok(out);
    }
    let settings = SettingsStore::new(cp.store().clone());
    for plugin in cp.plugins().list() {
        if plugin.extension.is_none() {
            continue;
        }
        let id = &plugin.manifest.id;
        if !cp
            .plugins()
            .is_enabled(&settings, id)
            .await
            .unwrap_or(false)
        {
            continue;
        }
        let snapshots = cp.extension_host().get(id).await;
        if snapshots.is_empty() {
            out.push(ExtensionStatusEntry {
                plugin_id: id.clone(),
                name: plugin.manifest.name.clone(),
                status: "not-running".to_string(),
                restart_count: 0,
                last_error: None,
                confirmed_events: Vec::new(),
                tool_count: 0,
            });
            continue;
        }
        for snap in snapshots {
            let (status, last_error) = match &snap.status {
                ExtensionStatus::Starting => ("starting".to_string(), None),
                ExtensionStatus::Running => ("running".to_string(), None),
                ExtensionStatus::Restarting => ("restarting".to_string(), None),
                ExtensionStatus::Stopped => ("stopped".to_string(), None),
                ExtensionStatus::Failed(reason) => ("failed".to_string(), Some(reason.clone())),
            };
            out.push(ExtensionStatusEntry {
                plugin_id: id.clone(),
                name: snap.name,
                status,
                restart_count: snap.restart_count,
                last_error,
                confirmed_events: snap.confirmed_events,
                tool_count: snap.tools.len() as u32,
            });
        }
    }
    out.sort_by(|a, b| {
        (a.plugin_id.as_str(), a.name.as_str()).cmp(&(b.plugin_id.as_str(), b.name.as_str()))
    });
    Ok(out)
}

#[cfg(test)]
mod tests {
    use crate::api::{dispatch, ApiState};
    use crate::plugins::extension::proc::SupervisedExtension;
    use crate::plugins::extension::{
        ExtensionCtx, ExtensionFactory, ExtensionSpec, ExtensionStatus,
    };
    use crate::plugins::Registries;
    use async_trait::async_trait;
    use serde_json::json;
    use std::sync::Arc;
    use std::time::Duration;

    /// Like `api::tests_support::state`, but seeded with `plugins` at
    /// `ControlPlane::new` time (that function doesn't take a `Registries`
    /// param, so every extension_status test needing a real extension-
    /// capable `CorePlugin` builds its own `ApiState` this way rather than
    /// reaching for a Registries mutator that doesn't exist post-construction).
    async fn state_with_plugins(plugins: Vec<crate::plugins::CorePlugin>) -> ApiState {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        let mut regs = Registries::new();
        for plugin in plugins {
            regs.add_plugin(plugin);
        }
        let cp = crate::control::ControlPlane::new(store, regs).await;
        std::mem::forget(tmp);
        ApiState {
            router_server: Arc::new(crate::llm_router::server::RouterServer::new(
                cp.store().clone(),
            )),
            cp,
            token: Some("t".into()),
        }
    }

    struct NoopExtensionFactory;
    #[async_trait]
    impl ExtensionFactory for NoopExtensionFactory {
        async fn extensions(&self, _ctx: &ExtensionCtx) -> anyhow::Result<Vec<ExtensionSpec>> {
            Ok(vec![])
        }
    }

    fn extension_plugin(id: &str) -> crate::plugins::CorePlugin {
        crate::plugins::CorePlugin {
            manifest: ryuzi_plugin_sdk::PluginManifest {
                contract: 1,
                id: id.to_string(),
                name: id.to_string(),
                version: String::new(),
                publisher: String::new(),
                description: String::new(),
                homepage: None,
                icon: None,
                categories: vec![],
                slot: None,
                verified: false,
                experimental: false,
                auth: None,
                settings: vec![],
                mcp: vec![],
                extensions: vec![],
                skills: vec![],
                provider: None,
            },
            harness: None,
            gateway: None,
            connector: None,
            extension: Some(std::sync::Arc::new(NoopExtensionFactory)),
            source: crate::plugins::PluginSource::Builtin,
        }
    }

    fn fake_spec(name: &str) -> ExtensionSpec {
        ExtensionSpec {
            name: name.to_string(),
            command: "unused-in-these-tests".to_string(),
            args: vec![],
            events: vec![],
            provides_tools: false,
            timeout: Duration::from_millis(500),
            env: vec![],
        }
    }

    #[tokio::test]
    async fn empty_host_reports_an_empty_list() {
        let s = state_with_plugins(vec![]).await;
        let out = dispatch(&s, "extension_status", json!({})).await.unwrap();
        assert_eq!(out, json!([]));
    }

    #[tokio::test]
    async fn reports_a_running_entry_with_confirmed_events_and_tool_count() {
        let s = state_with_plugins(vec![extension_plugin("acme-ext")]).await;
        s.cp.store()
            .set_setting_raw("plugin.acme-ext.enabled", "true")
            .await
            .unwrap();
        s.cp.extension_host()
            .insert_for_test(
                "acme-ext",
                SupervisedExtension::fixed_for_test(
                    fake_spec("linter"),
                    ExtensionStatus::Running,
                    vec!["tool.before".to_string()],
                    0,
                ),
            )
            .await;

        let out = dispatch(&s, "extension_status", json!({})).await.unwrap();
        let entries = out.as_array().unwrap();
        assert_eq!(entries.len(), 1, "got: {entries:?}");
        assert_eq!(entries[0]["pluginId"], json!("acme-ext"));
        assert_eq!(entries[0]["name"], json!("linter"));
        assert_eq!(entries[0]["status"], json!("running"));
        assert_eq!(entries[0]["restartCount"], json!(0));
        assert_eq!(entries[0]["lastError"], json!(null));
        assert_eq!(entries[0]["confirmedEvents"], json!(["tool.before"]));
        assert_eq!(entries[0]["toolCount"], json!(0));
    }

    #[tokio::test]
    async fn a_failed_entrys_last_error_is_the_sanitized_reason_never_a_raw_secret() {
        let s = state_with_plugins(vec![extension_plugin("secret-ext")]).await;
        s.cp.store()
            .set_setting_raw("plugin.secret-ext.enabled", "true")
            .await
            .unwrap();
        s.cp.extension_host()
            .insert_for_test(
                "secret-ext",
                SupervisedExtension::fixed_for_test(
                    fake_spec("linter"),
                    ExtensionStatus::Failed(
                        "linter: initialize protocol version mismatch".to_string(),
                    ),
                    vec![],
                    3,
                ),
            )
            .await;

        let out = dispatch(&s, "extension_status", json!({})).await.unwrap();
        let entries = out.as_array().unwrap();
        assert_eq!(entries.len(), 1, "got: {entries:?}");
        assert_eq!(entries[0]["status"], json!("failed"));
        assert_eq!(entries[0]["restartCount"], json!(3));
        let last_error = entries[0]["lastError"].as_str().unwrap();
        assert!(last_error.contains("linter"));
        // `sanitize_init_error` (proc.rs) collapses every handshake failure
        // to a canned per-stage message — it never echoes extension-supplied
        // text, so nothing token/secret-shaped can appear here regardless of
        // what a hostile extension's own JSON-RPC error body contained.
        assert!(!last_error.to_lowercase().contains("token"));
        assert!(!last_error.to_lowercase().contains("secret"));
    }

    #[tokio::test]
    async fn an_enabled_extension_plugin_with_nothing_spawned_reports_not_running_when_the_host_is_otherwise_active(
    ) {
        let s = state_with_plugins(vec![
            extension_plugin("unspawned-ext"),
            extension_plugin("sibling-ext"),
        ])
        .await;
        s.cp.store()
            .set_setting_raw("plugin.unspawned-ext.enabled", "true")
            .await
            .unwrap();
        s.cp.store()
            .set_setting_raw("plugin.sibling-ext.enabled", "true")
            .await
            .unwrap();
        s.cp.extension_host()
            .insert_for_test(
                "sibling-ext",
                SupervisedExtension::fixed_for_test(
                    fake_spec("linter"),
                    ExtensionStatus::Running,
                    vec![],
                    0,
                ),
            )
            .await;

        let out = dispatch(&s, "extension_status", json!({})).await.unwrap();
        let entries = out.as_array().unwrap();
        let unspawned = entries
            .iter()
            .find(|e| e["pluginId"] == json!("unspawned-ext"))
            .expect("unspawned-ext must have a synthetic not-running entry");
        assert_eq!(unspawned["status"], json!("not-running"));
        assert_eq!(unspawned["restartCount"], json!(0));
        assert_eq!(unspawned["lastError"], json!(null));
    }
}
