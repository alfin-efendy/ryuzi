//! Read-only plugin diagnostics: aggregates enable/auth/attach/binary issues
//! into a flat list of findings with a suggested action per finding. Never
//! mutates state and never leaks secrets.

use crate::control::ControlPlane;
use crate::plugins::extension::ExtensionStatus;
use crate::settings::SettingsStore;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorFinding {
    pub plugin_id: String,
    pub severity: String, // "warn" | "error"
    pub kind: String, // "reconnect-required" | "missing-binary" | "attach-failed" | "blocked" | "slot-conflict"
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

    // Extension (Track D "code plugin") findings are gated on the host
    // actually having spawned SOMETHING, for ANY plugin — an unconditionally
    // empty host means this control plane never called
    // `ExtensionHost::spawn_all` at all (every test `ControlPlane`, or a
    // process that isn't the daemon's spawn host), not that a specific
    // extension failed to start. Reporting `not-running` for every enabled
    // extension-capable plugin in that case would be a false positive, not a
    // real finding — this keeps the fresh-store/no-extensions invariant
    // (`doctor_is_empty_on_a_fresh_store_and_findings_are_secret_free`) true
    // without special-casing every extension-capable plugin's own enablement
    // check.
    let extensions_active = !cp.extension_host().is_empty().await;

    // Exclusive capability slot conflicts (Feature C2): a later claimant for
    // an already-owned `slot` never became owner (`PluginHost::add` — first
    // registration wins), but the arbitration should be observable rather
    // than silent. One finding per recorded conflict, attributed to the
    // losing plugin.
    for conflict in cp.plugins().slot_conflicts() {
        findings.push(DoctorFinding {
            plugin_id: conflict.loser_id.clone(),
            severity: "warn".into(),
            kind: "slot-conflict".into(),
            message: format!(
                "{} claims the `{}` slot, but {} already owns it — {}'s claim was ignored",
                conflict.loser_id, conflict.slot, conflict.winner_id, conflict.loser_id
            ),
            suggested_action: format!(
                "Uninstall/disable {} or {}, or change one plugin's manifest `slot`",
                conflict.winner_id, conflict.loser_id
            ),
        });
    }

    for plugin in cp.plugins().list() {
        let id = &plugin.manifest.id;

        // 0. Blocked by the remote catalog's signed feed (a revoked
        // integration) — always an error, regardless of enablement, since
        // an id can be blocked while `apply_blocked_denylist` hasn't yet run
        // (or a settings write failed) and it's still installed.
        let (blocked, reason) = crate::plugins::is_blocked(cp.store(), id).await;
        if blocked {
            findings.push(DoctorFinding {
                plugin_id: id.clone(),
                severity: "error".into(),
                kind: "blocked".into(),
                message: reason.unwrap_or_else(|| format!("{id} was revoked by the catalog")),
                suggested_action: format!("Uninstall or stop using {id} — it was revoked"),
            });
        }

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

        // 4. Extension (Track D "code plugin") runtime health — DT8. Only
        // meaningful once we know the host is actually spawning extensions
        // at all (see `extensions_active`'s doc above) and this plugin
        // declares `[[extension]]` and is enabled — mirrors
        // `ExtensionHost::spawn_all`'s own enablement gate exactly, so a
        // disabled extension plugin never gets a spurious `not-running`.
        if extensions_active
            && plugin.extension.is_some()
            && cp
                .plugins()
                .is_enabled(&settings, id)
                .await
                .unwrap_or(false)
        {
            let snapshots = cp.extension_host().get(id).await;
            if snapshots.is_empty() {
                // Enabled and the host is active elsewhere, but nothing was
                // ever spawned for THIS plugin (e.g. its `ExtensionFactory`
                // resolution failed, or a spawn is still pending).
                findings.push(DoctorFinding {
                    plugin_id: id.clone(),
                    severity: "warn".into(),
                    kind: "not-running".into(),
                    message: format!("{id} declares an extension, but none is currently running"),
                    suggested_action: format!("Restart the daemon or check {id}'s extension logs"),
                });
            }
            for snap in &snapshots {
                match &snap.status {
                    ExtensionStatus::Failed(reason) if reason.starts_with("restart-exhausted") => {
                        findings.push(DoctorFinding {
                            plugin_id: id.clone(),
                            severity: "error".into(),
                            kind: "restart-exhausted".into(),
                            message: format!(
                                "{id}'s extension `{}` gave up restarting: {reason}",
                                snap.name
                            ),
                            suggested_action: format!(
                                "Check {id}'s extension binary, then re-enable or reinstall {id}"
                            ),
                        });
                    }
                    ExtensionStatus::Failed(reason) => {
                        findings.push(DoctorFinding {
                            plugin_id: id.clone(),
                            severity: "error".into(),
                            kind: "init-failed".into(),
                            message: format!(
                                "{id}'s extension `{}` failed to start: {reason}",
                                snap.name
                            ),
                            suggested_action: format!(
                                "Check {id}'s extension binary and configuration"
                            ),
                        });
                    }
                    ExtensionStatus::Restarting => {
                        findings.push(DoctorFinding {
                            plugin_id: id.clone(),
                            severity: "warn".into(),
                            kind: "crashed".into(),
                            message: format!(
                                "{id}'s extension `{}` crashed and is restarting",
                                snap.name
                            ),
                            suggested_action: format!(
                                "Watch {id} — repeated crashes will exhaust its restart budget"
                            ),
                        });
                    }
                    // Running: healthy. Starting: mid-handshake, not yet
                    // resolved either way. Stopped: a graceful shutdown
                    // completed (daemon stop or an explicit disable), the
                    // expected terminal state — not a problem.
                    ExtensionStatus::Running
                    | ExtensionStatus::Starting
                    | ExtensionStatus::Stopped => {}
                }
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
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let mut regs = Registries::new();
        crate::plugins::install_builtins(&mut regs);
        {
            let persistence = crate::agents::bootstrap::AgentPersistence::temporary(store.clone())
                .await
                .unwrap();
            ControlPlane::new(store, regs, persistence).await
        }
    }

    /// A manifest-only plugin (no harness/gateway/connector capability)
    /// claiming `slot`, for exercising slot arbitration end to end through
    /// `plugin_doctor`.
    fn manifest_only_with_slot(id: &str, slot: &str) -> crate::plugins::CorePlugin {
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
                slot: Some(slot.to_string()),
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
            extension: None,
            provider: None,
            source: crate::plugins::PluginSource::Builtin,
        }
    }

    /// A control plane wired with two plugins that both claim the `memory`
    /// slot (no `install_builtins` noise — the embedded catalog's own
    /// `["memory"]`-categorized plugins never claim the slot, so a real
    /// conflict has to come from synthetic plugins like these two).
    async fn test_cp_with_slot_conflict() -> std::sync::Arc<ControlPlane> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
        let mut regs = Registries::new();
        regs.add_plugin(manifest_only_with_slot("mem0", "memory"));
        regs.add_plugin(manifest_only_with_slot("cavemem", "memory"));
        {
            let persistence = crate::agents::bootstrap::AgentPersistence::temporary(store.clone())
                .await
                .unwrap();
            ControlPlane::new(store, regs, persistence).await
        }
    }

    #[tokio::test]
    async fn doctor_reports_exactly_one_slot_conflict_finding_naming_winner_and_loser() {
        let cp = test_cp_with_slot_conflict().await;

        let findings = plugin_doctor(&cp).await.unwrap();
        let slot_findings: Vec<&DoctorFinding> = findings
            .iter()
            .filter(|f| f.kind == "slot-conflict")
            .collect();
        assert_eq!(
            slot_findings.len(),
            1,
            "exactly one slot-conflict finding, got: {findings:?}"
        );

        let finding = slot_findings[0];
        assert_eq!(finding.plugin_id, "cavemem", "attributed to the loser");
        assert_eq!(finding.severity, "warn");
        assert!(finding.message.contains("mem0"), "{}", finding.message);
        assert!(finding.message.contains("cavemem"), "{}", finding.message);
        assert!(finding.message.contains("memory"), "{}", finding.message);
        assert!(!finding.suggested_action.is_empty());
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

    // ---------- Extension (Track D) findings — DT8 ----------
    // These use `SupervisedExtension::fixed_for_test` +
    // `ExtensionHost::insert_for_test` (test-only, see `proc.rs`) to park a
    // host in an exact status deterministically, instead of racing a real
    // supervisor's restart-with-backoff timing (`restart-exhausted` alone
    // needs `MAX_RESTARTS_IN_WINDOW` real attempts in production).

    mod extension_findings {
        use super::*;
        use crate::plugins::extension::proc::SupervisedExtension;
        use crate::plugins::extension::{ExtensionCtx, ExtensionFactory, ExtensionSpec};
        use async_trait::async_trait;
        use std::time::Duration;

        struct NoopExtensionFactory;
        #[async_trait]
        impl ExtensionFactory for NoopExtensionFactory {
            async fn extensions(&self, _ctx: &ExtensionCtx) -> anyhow::Result<Vec<ExtensionSpec>> {
                Ok(vec![])
            }
        }

        /// An extension-capable, otherwise-empty `CorePlugin` — mirrors
        /// `plugins::host`'s and `plugins::extension::events`'s own
        /// `extension_only` test helpers. `ExtensionFactory::extensions` is
        /// never actually called by these tests (they seed the host directly
        /// via `insert_for_test` rather than `ExtensionHost::spawn_all`), so
        /// the factory's own behavior is irrelevant — only
        /// `CorePlugin.extension.is_some()` matters to `plugin_doctor`.
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
                provider: None,
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

        /// A control plane with one enabled, extension-capable plugin
        /// (`id`) registered and `plugin.<id>.enabled = true` persisted —
        /// the shared setup every test below builds on before seeding
        /// `cp.extension_host()` directly.
        async fn test_cp_with_enabled_extension_plugin(id: &str) -> std::sync::Arc<ControlPlane> {
            let tmp = tempfile::NamedTempFile::new().unwrap();
            let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
            let mut regs = Registries::new();
            regs.add_plugin(extension_plugin(id));
            let cp = {
                let persistence =
                    crate::agents::bootstrap::AgentPersistence::temporary(store.clone())
                        .await
                        .unwrap();
                ControlPlane::new(store, regs, persistence).await
            };
            cp.store()
                .set_setting_raw(&format!("plugin.{id}.enabled"), "true")
                .await
                .unwrap();
            cp
        }

        #[tokio::test]
        async fn restart_exhausted_extension_reports_one_error_finding_naming_the_plugin() {
            let cp = test_cp_with_enabled_extension_plugin("flaky-ext").await;
            cp.extension_host()
                .insert_for_test(
                    "flaky-ext",
                    SupervisedExtension::fixed_for_test(
                        fake_spec("linter"),
                        ExtensionStatus::Failed(
                            "restart-exhausted: 5 restarts within 300s".to_string(),
                        ),
                        vec![],
                        5,
                    ),
                )
                .await;

            let findings = plugin_doctor(&cp).await.unwrap();
            let ext_findings: Vec<&DoctorFinding> = findings
                .iter()
                .filter(|f| f.plugin_id == "flaky-ext")
                .collect();
            assert_eq!(
                ext_findings.len(),
                1,
                "exactly one finding for the exhausted extension, got: {findings:?}"
            );
            let finding = ext_findings[0];
            assert_eq!(finding.kind, "restart-exhausted");
            assert_eq!(finding.severity, "error");
            assert!(finding.message.contains("flaky-ext"), "{}", finding.message);
            assert!(!finding.suggested_action.is_empty());
        }

        #[tokio::test]
        async fn init_failed_extension_reports_one_error_finding() {
            let cp = test_cp_with_enabled_extension_plugin("broken-ext").await;
            cp.extension_host()
                .insert_for_test(
                    "broken-ext",
                    SupervisedExtension::fixed_for_test(
                        fake_spec("linter"),
                        ExtensionStatus::Failed(
                            "linter: initialize protocol version mismatch".to_string(),
                        ),
                        vec![],
                        0,
                    ),
                )
                .await;

            let findings = plugin_doctor(&cp).await.unwrap();
            let ext_findings: Vec<&DoctorFinding> = findings
                .iter()
                .filter(|f| f.plugin_id == "broken-ext")
                .collect();
            assert_eq!(ext_findings.len(), 1, "got: {findings:?}");
            let finding = ext_findings[0];
            assert_eq!(finding.kind, "init-failed");
            assert_eq!(finding.severity, "error");
            assert!(!finding.suggested_action.is_empty());
        }

        #[tokio::test]
        async fn restarting_extension_reports_one_warn_crashed_finding() {
            let cp = test_cp_with_enabled_extension_plugin("crashy-ext").await;
            cp.extension_host()
                .insert_for_test(
                    "crashy-ext",
                    SupervisedExtension::fixed_for_test(
                        fake_spec("linter"),
                        ExtensionStatus::Restarting,
                        vec![],
                        1,
                    ),
                )
                .await;

            let findings = plugin_doctor(&cp).await.unwrap();
            let ext_findings: Vec<&DoctorFinding> = findings
                .iter()
                .filter(|f| f.plugin_id == "crashy-ext")
                .collect();
            assert_eq!(ext_findings.len(), 1, "got: {findings:?}");
            assert_eq!(ext_findings[0].kind, "crashed");
            assert_eq!(ext_findings[0].severity, "warn");
        }

        #[tokio::test]
        async fn running_extension_reports_no_finding() {
            let cp = test_cp_with_enabled_extension_plugin("healthy-ext").await;
            cp.extension_host()
                .insert_for_test(
                    "healthy-ext",
                    SupervisedExtension::fixed_for_test(
                        fake_spec("linter"),
                        ExtensionStatus::Running,
                        vec!["tool.before".to_string()],
                        0,
                    ),
                )
                .await;

            let findings = plugin_doctor(&cp).await.unwrap();
            assert!(
                findings.iter().all(|f| f.plugin_id != "healthy-ext"),
                "a Running extension must produce no finding, got: {findings:?}"
            );
        }

        #[tokio::test]
        async fn enabled_extension_plugin_with_nothing_spawned_reports_not_running_when_the_host_is_otherwise_active(
        ) {
            // Two plugins: `unspawned-ext` is enabled+extension-capable but
            // the host never got an entry for it; `sibling-ext` IS spawned
            // (Running) so `ExtensionHost::is_empty()` is false — this is
            // what makes the host "otherwise active", the precondition for
            // `not-running` to mean anything (see `extensions_active`'s doc
            // on `plugin_doctor`).
            let tmp = tempfile::NamedTempFile::new().unwrap();
            let store = std::sync::Arc::new(crate::store::Store::open(tmp.path()).await.unwrap());
            let mut regs = Registries::new();
            regs.add_plugin(extension_plugin("unspawned-ext"));
            regs.add_plugin(extension_plugin("sibling-ext"));
            let cp = {
                let persistence =
                    crate::agents::bootstrap::AgentPersistence::temporary(store.clone())
                        .await
                        .unwrap();
                ControlPlane::new(store, regs, persistence).await
            };
            cp.store()
                .set_setting_raw("plugin.unspawned-ext.enabled", "true")
                .await
                .unwrap();
            cp.store()
                .set_setting_raw("plugin.sibling-ext.enabled", "true")
                .await
                .unwrap();
            cp.extension_host()
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

            let findings = plugin_doctor(&cp).await.unwrap();
            let ext_findings: Vec<&DoctorFinding> = findings
                .iter()
                .filter(|f| f.plugin_id == "unspawned-ext")
                .collect();
            assert_eq!(ext_findings.len(), 1, "got: {findings:?}");
            assert_eq!(ext_findings[0].kind, "not-running");
            assert_eq!(ext_findings[0].severity, "warn");
            assert!(
                findings.iter().all(|f| f.plugin_id != "sibling-ext"),
                "the spawned+Running sibling must produce no finding, got: {findings:?}"
            );
        }

        #[tokio::test]
        async fn an_enabled_extension_plugin_produces_no_finding_when_the_host_is_empty() {
            // The host was never spawned into at all (no `sibling-ext`-style
            // entry for ANY plugin) — this is the fresh-store / thin-client
            // case, and must stay silent rather than reporting `not-running`
            // for `lonely-ext`. Preserves the pre-DT8 invariant that a fresh
            // `ControlPlane` produces zero findings.
            let cp = test_cp_with_enabled_extension_plugin("lonely-ext").await;
            let findings = plugin_doctor(&cp).await.unwrap();
            assert!(
                findings.iter().all(|f| f.plugin_id != "lonely-ext"),
                "an empty host must never synthesize a not-running finding, got: {findings:?}"
            );
        }
    }
}
