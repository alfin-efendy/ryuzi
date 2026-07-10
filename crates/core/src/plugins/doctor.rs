//! Read-only plugin diagnostics: aggregates enable/auth/attach/binary issues
//! into a flat list of findings with a suggested action per finding. Never
//! mutates state and never leaks secrets.

use crate::control::ControlPlane;
use crate::settings::SettingsStore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorFinding {
    pub plugin_id: String,
    pub severity: String, // "warn" | "error"
    pub kind: String,     // "reconnect-required" | "missing-binary" | "attach-failed"
    pub message: String,
    pub suggested_action: String,
}

/// Aggregate plugin health into a flat list of findings, one per detected
/// issue. Read-only: it never mutates settings, the store, or any plugin
/// state, so it is safe to call from a diagnostic command or UI panel on any
/// cadence. Every `message`/`suggested_action` is secret-free — see this
/// module's callers (`ensure_auth` errors already never interpolate a raw
/// credential; the recorded attach `reason` is that same secret-free text).
pub async fn plugin_doctor(cp: &ControlPlane) -> anyhow::Result<Vec<DoctorFinding>> {
    let settings = SettingsStore::new(cp.store().clone());
    let mut findings = Vec::new();
    let attach = cp.store().list_plugin_attach().await.unwrap_or_default();

    for plugin in cp.plugins().list() {
        let id = &plugin.manifest.id;

        // 1. Enabled connector-only plugin with a missing stdio binary.
        if plugin.connector.is_some()
            && cp
                .plugins()
                .is_enabled(&settings, id)
                .await
                .unwrap_or(false)
        {
            for server in &plugin.manifest.mcp {
                if let Some(cmd) = server.command.as_deref() {
                    if !binary_on_path(cmd) {
                        findings.push(DoctorFinding {
                            plugin_id: id.clone(),
                            severity: "error".into(),
                            kind: "missing-binary".into(),
                            message: format!("{id} needs `{cmd}`, which is not on PATH"),
                            suggested_action: format!("Install `{cmd}` or disable {id}"),
                        });
                    }
                }
            }
        }

        // 2. OAuth token flagged reconnect_required.
        if let Ok(Some(tok)) = cp.store().get_plugin_oauth_token(id).await {
            if tok.reconnect_required {
                findings.push(DoctorFinding {
                    plugin_id: id.clone(),
                    severity: "warn".into(),
                    kind: "reconnect-required".into(),
                    message: format!("{id}'s sign-in expired"),
                    suggested_action: format!("Reconnect {id} in its plugin detail view"),
                });
            }
        }

        // 3. Last recorded attach failed.
        if let Some(a) = attach.iter().find(|a| &a.plugin_id == id) {
            if a.outcome == "failed" {
                findings.push(DoctorFinding {
                    plugin_id: id.clone(),
                    severity: "warn".into(),
                    kind: "attach-failed".into(),
                    message: a
                        .reason
                        .clone()
                        .unwrap_or_else(|| format!("{id} failed to attach")),
                    suggested_action: format!("Check {id}'s configuration"),
                });
            }
        }
    }
    Ok(findings)
}

/// Whether `cmd` resolves to an executable file: an absolute path or one
/// containing a `/` is checked directly; a bare name is scanned across
/// `PATH` (also trying `.exe`/`.cmd` suffixes for Windows-style shims).
fn binary_on_path(cmd: &str) -> bool {
    let candidate = std::path::Path::new(cmd);
    if candidate.is_absolute() || cmd.contains('/') {
        return candidate.is_file();
    }
    std::env::var_os("PATH")
        .map(|paths| {
            std::env::split_paths(&paths).any(|dir| {
                if dir.join(cmd).is_file() {
                    return true;
                }
                // Windows executable shims: APPEND (not replace) the
                // extension, so a dotted bare name like `python3.11` becomes
                // `python3.11.exe`, not the wrong `python3.exe` that
                // `Path::with_extension` would produce.
                ["exe", "cmd"]
                    .iter()
                    .any(|ext| dir.join(format!("{cmd}.{ext}")).is_file())
            })
        })
        .unwrap_or(false)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::plugins::Registries;

    /// A control plane wired with every embedded built-in/catalog plugin
    /// (`install_builtins`), backed by a throwaway on-disk SQLite file (the
    /// store's pooled connections need a real path, not `:memory:`).
    pub async fn test_cp_with_catalog() -> std::sync::Arc<ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = crate::store::Store::open(tmp.path()).await.unwrap();
        let mut regs = Registries::new();
        crate::plugins::install_builtins(&mut regs);
        ControlPlane::new(store, regs).await
    }

    #[tokio::test]
    async fn doctor_is_empty_on_a_fresh_store_and_findings_are_secret_free() {
        // A fresh control plane has no enabled connector plugins, no OAuth
        // tokens, and no attach rows, so doctor reports nothing. This is the
        // baseline; the seeded cases below prove the aggregation actually
        // fires. Any finding that *is* produced must still be secret-free.
        let cp = test_cp_with_catalog().await;
        let findings = plugin_doctor(&cp).await.unwrap();
        assert!(
            findings.is_empty(),
            "a fresh store should surface no findings, got: {findings:?}"
        );
        for f in &findings {
            assert!(!f.message.is_empty());
            assert!(!f.suggested_action.is_empty());
        }
    }

    #[tokio::test]
    async fn doctor_reports_a_seeded_attach_failure_without_leaking_raw_body() {
        // Seed a failed attach row (with an already-sanitized reason) for a
        // real catalog plugin id, then assert doctor surfaces an
        // `attach-failed` finding for it — this forces at least one real
        // finding so the secret-free/non-empty assertions actually run.
        let cp = test_cp_with_catalog().await;
        assert!(
            cp.plugins()
                .list()
                .iter()
                .any(|p| p.manifest.id == "github"),
            "test relies on the embedded `github` catalog plugin"
        );
        cp.store()
            .record_plugin_attach(&crate::store::PluginAttachStatus {
                plugin_id: "github".to_string(),
                last_attach_at: 1,
                outcome: "failed".to_string(),
                reason: Some("github: authentication failed".to_string()),
            })
            .await
            .unwrap();

        let findings = plugin_doctor(&cp).await.unwrap();
        let finding = findings
            .iter()
            .find(|f| f.plugin_id == "github" && f.kind == "attach-failed")
            .expect("doctor should report an attach-failed finding for the seeded row");
        assert_eq!(finding.severity, "warn");
        assert_eq!(finding.message, "github: authentication failed");
        assert!(!finding.suggested_action.is_empty());
        // The recorded reason is already sanitized upstream — doctor must
        // never surface anything token-like from an attach reason.
        for f in &findings {
            assert!(!f.message.is_empty());
            assert!(!f.suggested_action.is_empty());
            assert!(!f.message.contains("refresh_token"));
            assert!(!f.message.contains("client_secret"));
        }
    }

    #[tokio::test]
    async fn doctor_reports_a_reconnect_required_token() {
        // Seed an OAuth token flagged reconnect_required for a real catalog
        // plugin id and assert doctor surfaces the reconnect-required branch.
        // The token is encrypted at rest, so point the cipher at a hermetic
        // test key file (never the real keychain) before writing it.
        crate::llm_router::secrets::use_test_key_file();
        let cp = test_cp_with_catalog().await;
        cp.store()
            .upsert_plugin_oauth_token(&crate::plugins::oauth::PluginOauthToken {
                plugin_id: "linear".to_string(),
                access_token: "unused-in-this-test".to_string(),
                refresh_token: None,
                token_type: "Bearer".to_string(),
                expires_at: None,
                scopes: vec![],
                reconnect_required: true,
            })
            .await
            .unwrap();

        let findings = plugin_doctor(&cp).await.unwrap();
        let finding = findings
            .iter()
            .find(|f| f.plugin_id == "linear" && f.kind == "reconnect-required")
            .expect("doctor should report a reconnect-required finding for the seeded token");
        assert_eq!(finding.severity, "warn");
        assert!(!finding.message.is_empty());
        assert!(!finding.suggested_action.is_empty());
        assert!(!finding.message.contains("unused-in-this-test"));
    }

    #[test]
    fn binary_on_path_finds_a_binary_known_to_exist() {
        // `cargo` must be on PATH in the CI/dev environment running this test.
        assert!(binary_on_path("cargo") || binary_on_path("rustc"));
    }

    #[test]
    fn binary_on_path_rejects_a_nonexistent_bare_name() {
        assert!(!binary_on_path("definitely-not-a-real-binary-xyz"));
    }

    #[test]
    fn binary_on_path_checks_an_absolute_path_directly() {
        assert!(!binary_on_path("/definitely/not/a/real/path/xyz"));
    }
}
