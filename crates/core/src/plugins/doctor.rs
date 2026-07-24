//! Read-only plugin diagnostics: aggregates enable/auth/attach/binary issues
//! into a flat list of findings with a suggested action per finding. Never
//! mutates state and never leaks secrets.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::control::ControlPlane;
use crate::gateway::GatewayRestartHealth;
use crate::plugins::bundle::InstalledBundle;
use crate::plugins::extension::ExtensionStatus;
use crate::plugins::runtime::HostPolicy;
use crate::settings::SettingsStore;
use crate::store::{ComponentPluginReleaseRecord, Store};
use ryuzi_plugin_sdk::{PluginBundleManifest, PluginRelease};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// The WIT contract version this host implements — the single concrete version
/// every `ryuzi:*@0.1.0` import/export in [`crate::plugins::runtime`] is pinned
/// to, and the value the first-party signer stamps into each `release.json`
/// (`scripts/plugins/build-first-party.ts`'s `WIT_API_VERSION`). A component
/// whose manifest `wit-api` range excludes this version cannot bind against the
/// host — see the `abi-incompatible` finding.
const HOST_WIT_API_VERSION: &str = "0.1.0";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorFinding {
    pub plugin_id: String,
    pub severity: String, // "warn" | "error"
    // "reconnect-required" | "missing-binary" | "attach-failed" | "blocked" |
    // "slot-conflict" | "not-running" | "crashed" | "restart-exhausted" |
    // "init-failed" | "signature-invalid" | "hash-mismatch" | "abi-incompatible"
    // | "revoked" | "policy-violation" | "oauth-profile-unhealthy" |
    // "gateway-restart-exhausted"
    pub kind: String,
    pub message: String,
    pub suggested_action: String,
}

/// The extra state the WASM-component lifecycle diagnostics read, injected so a
/// caller (and tests) can point them at a specific install root, trusted-key
/// set, and gateway health snapshot. [`plugin_doctor`] fills these from the
/// real per-user install root, the compiled-in first-party trusted keys, and
/// the control plane's live gateways.
pub struct WasmComponentDoctorInputs {
    /// Where installed component bundles live on disk (production:
    /// [`crate::plugins::bundle::installed_bundle_root`]).
    pub bundle_root: PathBuf,
    /// Trusted bundle-signing keys, keyed by `key_id` — the same map
    /// [`crate::plugins::bundle::verify_bundle`] takes. Production:
    /// [`crate::plugins::first_party_key::first_party_trusted_keys`], which is
    /// EMPTY while the placeholder key is in place; signature re-verification is
    /// skipped when it is empty (there is nothing to verify against).
    pub trusted_keys: HashMap<String, [u8; 32]>,
    /// Live `(gateway_id, restart health)` for every running gateway that has a
    /// supervised restart budget (production: [`ControlPlane::gateway_restart_health`]).
    pub gateway_health: Vec<(String, GatewayRestartHealth)>,
}

/// Aggregate plugin health into a flat list of findings, one per detected
/// issue. Read-only: it never mutates settings, the store, or any plugin
/// state, so it is safe to call from a diagnostic command or UI panel on any
/// cadence. Every `message`/`suggested_action` is secret-free — see this
/// module's callers (`ensure_auth` errors already never interpolate a raw
/// credential; the recorded attach `reason` is that same secret-free text).
pub async fn plugin_doctor(cp: &ControlPlane) -> anyhow::Result<Vec<DoctorFinding>> {
    let wasm = WasmComponentDoctorInputs {
        bundle_root: crate::plugins::bundle::installed_bundle_root(),
        trusted_keys: crate::plugins::first_party_key::first_party_trusted_keys(),
        gateway_health: cp.gateway_restart_health(),
    };
    plugin_doctor_with(cp, &wasm).await
}

/// [`plugin_doctor`] with the WASM-component lifecycle inputs supplied
/// explicitly, so tests can drive the component/gateway findings against a
/// hermetic install root, trusted-key set, and gateway snapshot instead of the
/// real per-user state. Production always goes through [`plugin_doctor`].
pub async fn plugin_doctor_with(
    cp: &ControlPlane,
    wasm: &WasmComponentDoctorInputs,
) -> anyhow::Result<Vec<DoctorFinding>> {
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

    // WASM-component release + long-lived-gateway lifecycle health (Task 17b).
    append_wasm_component_findings(&mut findings, cp.store(), wasm).await;

    Ok(findings)
}

/// Append the WASM-component lifecycle findings (Task 17b): per installed
/// component release — signature, hash, ABI compatibility, ledger revoke, and
/// policy/OAuth health — plus long-lived WASM-gateway restart exhaustion. The
/// installed set is driven off the component ledger
/// ([`Store::list_component_release_plugin_ids`]) so a fresh store surfaces
/// nothing, and every finding is GENERIC (iterates installed components; no
/// plugin-id branch) and SECRET-FREE (messages never carry a token, key, or
/// credential fragment). All reads are best-effort: a store or filesystem error
/// warn-and-skips rather than failing the whole diagnostic.
async fn append_wasm_component_findings(
    findings: &mut Vec<DoctorFinding>,
    store: &Store,
    wasm: &WasmComponentDoctorInputs,
) {
    let plugin_ids = store
        .list_component_release_plugin_ids()
        .await
        .unwrap_or_default();
    for id in &plugin_ids {
        // Ledger-level revoke (distinct from the signed-feed `blocked`
        // finding): report every recorded-revoked release, disk-independent.
        let releases = store.list_component_releases(id).await.unwrap_or_default();
        for release in &releases {
            if release.revoked {
                findings.push(revoked_finding(release));
            }
        }

        // The remaining checks are about the currently-ACTIVE release's
        // on-disk bundle; a plugin with no active release (all revoked/rolled
        // back) contributes only the ledger findings above.
        let Some(active) = store.active_component_release(id).await.ok().flatten() else {
            continue;
        };
        let version_dir = wasm
            .bundle_root
            .join(&active.plugin_id)
            .join(&active.version);
        let Some(loaded) = load_installed_release(&version_dir, &active) else {
            continue;
        };

        // Hash first: a tampered component fails signature verification too, so
        // reporting the hash mismatch alone (and skipping signature) avoids a
        // duplicate finding for the same corruption.
        let hash_ok = match append_hash_finding(findings, &active, &loaded.component_path) {
            HashOutcome::Matches => true,
            HashOutcome::MismatchReported | HashOutcome::Unreadable => false,
        };
        // A structurally-invalid manifest is surfaced as `policy-violation`
        // below; skip signature re-verification in that case (it would fail
        // inside `verify_bundle`'s own manifest parse and mislabel the manifest
        // defect as a signature defect).
        let manifest_valid = loaded.manifest.validate().is_ok();
        if hash_ok && manifest_valid {
            append_signature_finding(findings, &active, &version_dir, &wasm.trusted_keys);
        }
        append_abi_finding(findings, &active, &loaded.manifest);
        append_policy_findings(findings, &loaded);
        append_oauth_profile_findings(findings, store, &loaded.manifest).await;
    }

    // Long-lived WASM gateways stuck restarting (distinct from the Track-D
    // extension `restart-exhausted`; both are kept).
    for (gateway_id, health) in &wasm.gateway_health {
        if let Some(finding) = gateway_restart_exhausted_finding(gateway_id, *health) {
            findings.push(finding);
        }
    }
}

/// A parsed, on-disk installed component release: enough to run the signature,
/// hash, ABI, policy, and OAuth checks without re-reading the directory each
/// time. Loaded tolerantly by [`load_installed_release`] so a missing or
/// corrupt install directory warn-and-skips the file-based findings rather than
/// aborting the whole diagnostic.
struct LoadedRelease {
    manifest: PluginBundleManifest,
    release: PluginRelease,
    release_record: ComponentPluginReleaseRecord,
    root: PathBuf,
    component_path: PathBuf,
}

/// Read `<version_dir>/{ryuzi-plugin.toml, release.json}` and resolve the
/// declared component path, WITHOUT re-verifying hash or signature (those are
/// their own findings). Returns `None` if any file is missing/unparseable — a
/// warn-and-skip: the ledger findings for this plugin were already emitted, and
/// a corrupt active install simply yields no file-based finding rather than a
/// crash. The manifest is parsed leniently (no `validate()` short-circuit) so a
/// structurally-invalid-but-parseable manifest still reaches the
/// `policy-violation` check.
fn load_installed_release(
    version_dir: &Path,
    record: &ComponentPluginReleaseRecord,
) -> Option<LoadedRelease> {
    let manifest_toml = std::fs::read_to_string(version_dir.join("ryuzi-plugin.toml")).ok()?;
    let manifest: PluginBundleManifest = toml::from_str(&manifest_toml).ok()?;
    let release_bytes = std::fs::read(version_dir.join("release.json")).ok()?;
    let release = PluginRelease::from_json(&release_bytes).ok()?;
    let component_path = version_dir.join(&manifest.component);
    Some(LoadedRelease {
        manifest,
        release,
        release_record: record.clone(),
        root: version_dir.to_path_buf(),
        component_path,
    })
}

/// The outcome of the on-disk component hash check.
enum HashOutcome {
    /// The on-disk bytes hash to the recorded checksum.
    Matches,
    /// A mismatch was found and a `hash-mismatch` finding was pushed.
    MismatchReported,
    /// The component file could not be read (its absence is not itself a
    /// hash-tamper finding — the file-based checks simply cannot run).
    Unreadable,
}

/// Finding #2 — hash. SHA-256 the on-disk component (the same lowercase-hex
/// hashing [`crate::plugins::bundle`] uses) and compare it to the ledger's
/// recorded `sha256`, the tamper-evident anchor. A mismatch means the installed
/// component was mutated or corrupted after installation.
fn append_hash_finding(
    findings: &mut Vec<DoctorFinding>,
    record: &ComponentPluginReleaseRecord,
    component_path: &Path,
) -> HashOutcome {
    let Ok(bytes) = std::fs::read(component_path) else {
        return HashOutcome::Unreadable;
    };
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual == record.sha256 {
        return HashOutcome::Matches;
    }
    findings.push(DoctorFinding {
        plugin_id: record.plugin_id.clone(),
        severity: "error".into(),
        kind: "hash-mismatch".into(),
        message: format!(
            "{}'s installed component ({}) no longer matches its recorded checksum — possible tampering or corruption",
            record.plugin_id, record.version
        ),
        suggested_action: format!(
            "Reinstall {} or roll it back to a known-good version",
            record.plugin_id
        ),
    });
    HashOutcome::MismatchReported
}

/// Finding #1 — signature. Re-verify the installed active release against the
/// trusted keys, reusing [`crate::plugins::bundle::verify_bundle`] (never a
/// hand-rolled ed25519 path). An unknown/untrusted `signing_key_id` is reported
/// directly; otherwise a `verify_bundle` failure (bad signature, or a mutated
/// `release.json` that invalidates the signed bytes) is reported. Skipped when
/// `trusted_keys` is empty (the fail-closed placeholder state — there is
/// nothing to verify against, and nothing could have installed either). The
/// message names the plugin/version and the reason, never the key material.
fn append_signature_finding(
    findings: &mut Vec<DoctorFinding>,
    record: &ComponentPluginReleaseRecord,
    version_dir: &Path,
    trusted_keys: &HashMap<String, [u8; 32]>,
) {
    if trusted_keys.is_empty() {
        return;
    }
    let reason = if !trusted_keys.contains_key(&record.signing_key_id) {
        // `signing_key_id` is a key IDENTIFIER (e.g. `first-party`), not key
        // material — safe to name; it is what makes the finding actionable.
        Some(format!(
            "it is signed by an untrusted key id `{}`",
            record.signing_key_id
        ))
    } else {
        match crate::plugins::bundle::verify_bundle(version_dir, trusted_keys) {
            Ok(_) => None,
            Err(_) => Some("its bundle signature no longer verifies".to_string()),
        }
    };
    if let Some(reason) = reason {
        findings.push(DoctorFinding {
            plugin_id: record.plugin_id.clone(),
            severity: "error".into(),
            kind: "signature-invalid".into(),
            message: format!(
                "{} ({}) failed signature verification: {reason}",
                record.plugin_id, record.version
            ),
            suggested_action: format!("Reinstall {} from a trusted source", record.plugin_id),
        });
    }
}

/// Finding #3 — ABI compatibility. Check the installed release's manifest
/// `wit-api` RANGE against the host's supported WIT contract version
/// ([`HOST_WIT_API_VERSION`]) using [`semver::VersionReq`] — never a hand-rolled
/// range parser. A release whose accepted range excludes the running host
/// version cannot bind its imports/exports against this host.
fn append_abi_finding(
    findings: &mut Vec<DoctorFinding>,
    record: &ComponentPluginReleaseRecord,
    manifest: &PluginBundleManifest,
) {
    let (Ok(req), Ok(host)) = (
        semver::VersionReq::parse(&manifest.wit_api),
        semver::Version::parse(HOST_WIT_API_VERSION),
    ) else {
        return;
    };
    if req.matches(&host) {
        return;
    }
    findings.push(DoctorFinding {
        plugin_id: record.plugin_id.clone(),
        severity: "error".into(),
        kind: "abi-incompatible".into(),
        message: format!(
            "{} ({}) targets WIT contract `{}`, which excludes this host's `{HOST_WIT_API_VERSION}`",
            record.plugin_id, record.version, manifest.wit_api
        ),
        suggested_action: format!(
            "Update {} to a release built for WIT `{HOST_WIT_API_VERSION}`",
            record.plugin_id
        ),
    });
}

/// Finding #5 — policy violations, defined concretely from the EXISTING
/// manifest + host-policy machinery (no new policy engine):
///
/// 1. Structural: the installed manifest fails [`PluginBundleManifest::validate`]
///    (e.g. a network allowlist entry that is not a valid host). `error`.
/// 2. Grant mismatch: the manifest declares router `provider-ids` (requesting
///    the `ryuzi:provider-auth` capability) but [`HostPolicy::for_installed_bundle`]
///    will not grant `allow_provider_auth` — because provider credential
///    injection also requires a declared network allowlist. A declared
///    capability with no corresponding grant. `warn`.
fn append_policy_findings(findings: &mut Vec<DoctorFinding>, loaded: &LoadedRelease) {
    let id = &loaded.manifest.id;
    let version = &loaded.manifest.version;

    if let Err(error) = loaded.manifest.validate() {
        findings.push(DoctorFinding {
            plugin_id: id.clone(),
            severity: "error".into(),
            kind: "policy-violation".into(),
            // `BundleError` renders only ids/hosts/versions, never a secret.
            message: format!("{id} ({version})'s manifest is invalid: {error}"),
            suggested_action: format!("Reinstall {id} with a corrected manifest"),
        });
    }

    let bundle = InstalledBundle {
        manifest: loaded.manifest.clone(),
        release: loaded.release.clone(),
        release_record: loaded.release_record.clone(),
        root: loaded.root.clone(),
        component_path: loaded.component_path.clone(),
    };
    let policy = HostPolicy::for_installed_bundle(&bundle);
    if !loaded.manifest.provider_ids.is_empty() && !policy.allow_provider_auth {
        findings.push(DoctorFinding {
            plugin_id: id.clone(),
            severity: "warn".into(),
            kind: "policy-violation".into(),
            message: format!(
                "{id} ({version}) declares provider ids but the host cannot grant provider credential injection without a network allowlist"
            ),
            suggested_action: format!(
                "Add the required network hosts to {id}'s manifest, or remove its provider ids"
            ),
        });
    }
}

/// Finding #6 — OAuth profile health. For each `[[oauth]]` profile the installed
/// manifest declares, inspect the PROFILE-scoped token
/// ([`Store::get_plugin_oauth_profile_token`], the Task-8 profile model — not
/// the legacy `plugin_oauth_token` the `reconnect-required` finding covers). A
/// missing, reconnect-flagged, or expired token is unhealthy. The token itself
/// is NEVER read into the message.
async fn append_oauth_profile_findings(
    findings: &mut Vec<DoctorFinding>,
    store: &Store,
    manifest: &PluginBundleManifest,
) {
    let id = &manifest.id;
    for profile in &manifest.oauth {
        let reason = match store.get_plugin_oauth_profile_token(id, &profile.id).await {
            Ok(None) => Some("it is not connected (no stored token)"),
            Ok(Some(token)) if token.reconnect_required => Some("its sign-in needs reconnecting"),
            Ok(Some(token)) => match token.expires_at {
                Some(expires_at) if expires_at <= crate::paths::now_ms() => {
                    Some("its stored token has expired")
                }
                _ => None,
            },
            // A decode/store error is not a health claim we can make safely.
            Err(_) => None,
        };
        if let Some(reason) = reason {
            findings.push(DoctorFinding {
                plugin_id: id.clone(),
                severity: "warn".into(),
                kind: "oauth-profile-unhealthy".into(),
                message: format!(
                    "{id}'s OAuth profile `{}` is unhealthy: {reason}",
                    profile.id
                ),
                suggested_action: format!("Reconnect {id}'s `{}` profile", profile.id),
            });
        }
    }
}

/// Finding #4 — ledger revoke. A recorded-revoked release from the component
/// ledger, carrying its (operator-authored, secret-free) `revocation_reason`
/// when present. Distinct from the signed-feed `blocked` finding.
fn revoked_finding(record: &ComponentPluginReleaseRecord) -> DoctorFinding {
    let detail = record
        .revocation_reason
        .as_deref()
        .map(|reason| format!(": {reason}"))
        .unwrap_or_default();
    DoctorFinding {
        plugin_id: record.plugin_id.clone(),
        severity: "error".into(),
        kind: "revoked".into(),
        message: format!(
            "{} ({}) was revoked in the component ledger{detail}",
            record.plugin_id, record.version
        ),
        suggested_action: format!(
            "Uninstall {} or roll it back to a non-revoked version",
            record.plugin_id
        ),
    }
}

/// Finding #7 — long-lived gateway restart exhaustion. A supervised WASM
/// gateway that is NOT running while its restart count has already reached the
/// backoff ceiling ([`crate::plugins::wasm_gateway::GATEWAY_BACKOFF_CEILING_RESTARTS`])
/// is stuck retrying as slowly as it ever will — effectively down. Distinct
/// from the Track-D extension `restart-exhausted`.
fn gateway_restart_exhausted_finding(
    gateway_id: &str,
    health: GatewayRestartHealth,
) -> Option<DoctorFinding> {
    let exhausted = !health.running
        && health.restart_count >= crate::plugins::wasm_gateway::GATEWAY_BACKOFF_CEILING_RESTARTS;
    if !exhausted {
        return None;
    }
    Some(DoctorFinding {
        plugin_id: gateway_id.to_string(),
        severity: "error".into(),
        kind: "gateway-restart-exhausted".into(),
        message: format!(
            "{gateway_id}'s gateway is down after {} restarts and is retrying at its backoff ceiling",
            health.restart_count
        ),
        suggested_action: format!(
            "Check {gateway_id}'s gateway configuration/connectivity, then re-enable or reinstall it"
        ),
    })
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

    // ---------- WASM-component lifecycle findings — Task 17b ----------
    // Each test seeds the minimal ledger + on-disk bundle state that triggers
    // one finding, driving `plugin_doctor_with` against a hermetic install
    // root/trusted-key set/gateway snapshot. Every seeded token/key material is
    // asserted absent from the emitted messages.
    mod wasm_component_findings {
        use super::*;
        use crate::gateway::GatewayRestartHealth;
        use crate::plugins::oauth::PluginOauthToken;
        use crate::plugins::wasm_gateway::GATEWAY_BACKOFF_CEILING_RESTARTS;
        use ed25519_dalek::{Signer, SigningKey};
        use std::fs;
        use std::sync::Arc;

        const FIRST_PARTY: &str = "first-party";

        /// The trusted first-party signer; its `key_id` is `first-party`.
        fn signing_key() -> SigningKey {
            SigningKey::from_bytes(&[9u8; 32])
        }

        /// A DIFFERENT key, never in the trusted set — for the untrusted-key and
        /// bad-signature signature cases.
        fn rogue_key() -> SigningKey {
            SigningKey::from_bytes(&[3u8; 32])
        }

        fn trusted_keys() -> HashMap<String, [u8; 32]> {
            let mut map = HashMap::new();
            map.insert(
                FIRST_PARTY.to_string(),
                signing_key().verifying_key().to_bytes(),
            );
            map
        }

        fn b64url(bytes: &[u8]) -> String {
            use base64::Engine as _;
            base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
        }

        /// A control plane over a throwaway store, plus that same store handle so
        /// a test can seed the component ledger the doctor reads.
        async fn cp_with_store() -> (Arc<ControlPlane>, Arc<Store>, tempfile::NamedTempFile) {
            let tmp = tempfile::NamedTempFile::new().unwrap();
            let store = Arc::new(Store::open(tmp.path()).await.unwrap());
            let regs = Registries::new();
            let persistence = crate::agents::bootstrap::AgentPersistence::temporary(store.clone())
                .await
                .unwrap();
            let cp = ControlPlane::new(store.clone(), regs, persistence).await;
            (cp, store, tmp)
        }

        fn wasm_inputs(
            root: &Path,
            trusted_keys: HashMap<String, [u8; 32]>,
            gateway_health: Vec<(String, GatewayRestartHealth)>,
        ) -> WasmComponentDoctorInputs {
            WasmComponentDoctorInputs {
                bundle_root: root.to_path_buf(),
                trusted_keys,
                gateway_health,
            }
        }

        /// A bundle manifest with `component = "plugin.wasm"` and `body` appended
        /// (e.g. an `[[oauth]]` block or `provider-ids`).
        fn manifest(id: &str, version: &str, wit_api: &str, body: &str) -> String {
            format!(
                "id = \"{id}\"\nname = \"{id}\"\nversion = \"{version}\"\nwit-api = \"{wit_api}\"\nlifecycle = \"singleton\"\ncomponent = \"plugin.wasm\"\n{body}"
            )
        }

        fn release_json(id: &str, version: &str, wit_api: &str, sha: &str) -> Vec<u8> {
            format!(
                "{{\"id\":\"{id}\",\"version\":\"{version}\",\"wit-api\":\"{wit_api}\",\"component_url\":\"https://registry.example.com/{id}/{version}/plugin.wasm\",\"component_sha256\":\"{sha}\"}}"
            )
            .into_bytes()
        }

        /// Write a signed `<root>/<id>/<version>/` bundle dir (+ `current`
        /// pointer) exactly as the installer leaves it, returning the component
        /// sha256. `release_wit_api` is the release descriptor's concrete WIT
        /// version; the manifest's own `wit-api` RANGE comes from `manifest_toml`.
        // A test fixture writer whose parameters each pick one independent axis
        // of the bundle to vary; grouping them into a struct would only obscure
        // the call sites. Mirrors `wasm_gateway::supervise`'s own allow.
        #[allow(clippy::too_many_arguments)]
        fn write_bundle(
            root: &Path,
            id: &str,
            version: &str,
            manifest_toml: &str,
            component: &[u8],
            release_wit_api: &str,
            key: &SigningKey,
            sig_key_id: &str,
        ) -> String {
            let plugin_root = root.join(id);
            let version_dir = plugin_root.join(version);
            fs::create_dir_all(&version_dir).unwrap();
            fs::write(version_dir.join("plugin.wasm"), component).unwrap();
            let sha = format!("{:x}", Sha256::digest(component));
            fs::write(version_dir.join("ryuzi-plugin.toml"), manifest_toml).unwrap();
            let release_bytes = release_json(id, version, release_wit_api, &sha);
            fs::write(version_dir.join("release.json"), &release_bytes).unwrap();
            let signature = key.sign(&release_bytes);
            let envelope = serde_json::json!({
                "key_id": sig_key_id,
                "signature": b64url(&signature.to_bytes()),
            });
            fs::write(
                version_dir.join("plugin.sig"),
                serde_json::to_vec(&envelope).unwrap(),
            )
            .unwrap();
            fs::write(plugin_root.join("current"), version).unwrap();
            sha
        }

        fn release_record(
            id: &str,
            version: &str,
            sha: &str,
            signing_key_id: &str,
        ) -> ComponentPluginReleaseRecord {
            ComponentPluginReleaseRecord {
                plugin_id: id.into(),
                version: version.into(),
                source_url: "https://registry.example.com/x".into(),
                sha256: sha.into(),
                signing_key_id: signing_key_id.into(),
                installed_at: 1,
                active: false,
                revoked: false,
                revocation_reason: None,
            }
        }

        /// Ledger a release and mark it the active one for its plugin.
        async fn seed_active(store: &Store, id: &str, version: &str, sha: &str, key_id: &str) {
            store
                .upsert_component_release(&release_record(id, version, sha, key_id))
                .await
                .unwrap();
            store
                .set_active_component_release(id, version)
                .await
                .unwrap();
        }

        #[tokio::test]
        async fn healthy_installed_release_yields_no_component_findings() {
            let (cp, store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            let m = manifest("acme-connector", "0.1.0", "^0.1.0", "");
            let sha = write_bundle(
                root.path(),
                "acme-connector",
                "0.1.0",
                &m,
                b"component bytes",
                "0.1.0",
                &signing_key(),
                FIRST_PARTY,
            );
            seed_active(&store, "acme-connector", "0.1.0", &sha, FIRST_PARTY).await;

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), trusted_keys(), vec![]))
                    .await
                    .unwrap();
            assert!(
                findings.is_empty(),
                "a fully valid installed release must yield no finding, got: {findings:?}"
            );
        }

        #[tokio::test]
        async fn tampered_component_reports_hash_mismatch_and_suppresses_signature() {
            let (cp, store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            let m = manifest("acme-connector", "0.1.0", "^0.1.0", "");
            let sha = write_bundle(
                root.path(),
                "acme-connector",
                "0.1.0",
                &m,
                b"original bytes",
                "0.1.0",
                &signing_key(),
                FIRST_PARTY,
            );
            seed_active(&store, "acme-connector", "0.1.0", &sha, FIRST_PARTY).await;
            // Mutate the installed component after the checksum was recorded.
            fs::write(
                root.path().join("acme-connector/0.1.0/plugin.wasm"),
                b"tampered after install",
            )
            .unwrap();

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), trusted_keys(), vec![]))
                    .await
                    .unwrap();
            let finding = findings
                .iter()
                .find(|f| f.kind == "hash-mismatch")
                .expect("a mutated component must report hash-mismatch");
            assert_eq!(finding.plugin_id, "acme-connector");
            assert_eq!(finding.severity, "error");
            assert!(!finding.suggested_action.is_empty());
            assert!(
                findings.iter().all(|f| f.kind != "signature-invalid"),
                "a hash mismatch must suppress the signature finding for the same corruption: {findings:?}"
            );
        }

        #[tokio::test]
        async fn untrusted_signing_key_reports_signature_invalid_without_leaking_key_material() {
            let (cp, store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            let m = manifest("acme-connector", "0.1.0", "^0.1.0", "");
            let sha = write_bundle(
                root.path(),
                "acme-connector",
                "0.1.0",
                &m,
                b"component bytes",
                "0.1.0",
                &rogue_key(),
                "rogue-key",
            );
            seed_active(&store, "acme-connector", "0.1.0", &sha, "rogue-key").await;

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), trusted_keys(), vec![]))
                    .await
                    .unwrap();
            let finding = findings
                .iter()
                .find(|f| f.kind == "signature-invalid")
                .expect("an untrusted signing key must report signature-invalid");
            assert_eq!(finding.plugin_id, "acme-connector");
            assert_eq!(finding.severity, "error");
            // The key IDENTIFIER is named (actionable); the key MATERIAL is not.
            let rogue_pubkey_hex = hex_of(&rogue_key().verifying_key().to_bytes());
            assert!(
                !finding.message.contains(&rogue_pubkey_hex),
                "signature finding must never contain key material"
            );
        }

        #[tokio::test]
        async fn bad_signature_from_a_trusted_key_id_reports_signature_invalid() {
            let (cp, store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            let m = manifest("acme-connector", "0.1.0", "^0.1.0", "");
            // Envelope names the trusted `first-party` key id, but the bytes were
            // signed by a different key — so `verify_bundle` fails in
            // `verify_strict`, not on an unknown key id.
            let sha = write_bundle(
                root.path(),
                "acme-connector",
                "0.1.0",
                &m,
                b"component bytes",
                "0.1.0",
                &rogue_key(),
                FIRST_PARTY,
            );
            seed_active(&store, "acme-connector", "0.1.0", &sha, FIRST_PARTY).await;

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), trusted_keys(), vec![]))
                    .await
                    .unwrap();
            let finding = findings
                .iter()
                .find(|f| f.kind == "signature-invalid")
                .expect("a signature that no longer verifies must report signature-invalid");
            assert_eq!(finding.severity, "error");
            assert!(
                finding.message.contains("no longer verifies"),
                "{}",
                finding.message
            );
        }

        #[tokio::test]
        async fn signature_check_is_skipped_when_the_trusted_set_is_empty() {
            // The fail-closed placeholder state: an empty trusted set has nothing
            // to verify against, so the doctor must not flag every install.
            let (cp, store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            let m = manifest("acme-connector", "0.1.0", "^0.1.0", "");
            let sha = write_bundle(
                root.path(),
                "acme-connector",
                "0.1.0",
                &m,
                b"component bytes",
                "0.1.0",
                &rogue_key(),
                "rogue-key",
            );
            seed_active(&store, "acme-connector", "0.1.0", &sha, "rogue-key").await;

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), HashMap::new(), vec![]))
                    .await
                    .unwrap();
            assert!(
                findings.iter().all(|f| f.kind != "signature-invalid"),
                "an empty trusted set must skip signature verification, got: {findings:?}"
            );
        }

        #[tokio::test]
        async fn release_targeting_incompatible_wit_reports_abi_incompatible() {
            let (cp, store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            // Manifest targets `^0.2.0`, which excludes the host's `0.1.0`.
            let m = manifest("acme-connector", "0.1.0", "^0.2.0", "");
            let sha = write_bundle(
                root.path(),
                "acme-connector",
                "0.1.0",
                &m,
                b"component bytes",
                "0.1.0",
                &signing_key(),
                FIRST_PARTY,
            );
            seed_active(&store, "acme-connector", "0.1.0", &sha, FIRST_PARTY).await;

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), trusted_keys(), vec![]))
                    .await
                    .unwrap();
            let finding = findings
                .iter()
                .find(|f| f.kind == "abi-incompatible")
                .expect("a WIT range excluding the host version must report abi-incompatible");
            assert_eq!(finding.severity, "error");
            assert!(finding.message.contains("0.1.0"), "{}", finding.message);
            assert!(finding.message.contains("^0.2.0"), "{}", finding.message);
        }

        #[tokio::test]
        async fn revoked_ledger_release_reports_revoked_with_its_reason() {
            let (cp, store, _db) = cp_with_store().await;
            // No on-disk bundle needed: the revoke finding is ledger-driven.
            let root = tempfile::tempdir().unwrap();
            store
                .upsert_component_release(&release_record(
                    "acme-connector",
                    "0.1.0",
                    "0".repeat(64).as_str(),
                    FIRST_PARTY,
                ))
                .await
                .unwrap();
            store
                .mark_component_release_revoked(
                    "acme-connector",
                    "0.1.0",
                    "superseded by a security fix",
                )
                .await
                .unwrap();

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), trusted_keys(), vec![]))
                    .await
                    .unwrap();
            let finding = findings
                .iter()
                .find(|f| f.kind == "revoked")
                .expect("a ledger-revoked release must report revoked");
            assert_eq!(finding.plugin_id, "acme-connector");
            assert_eq!(finding.severity, "error");
            assert!(
                finding.message.contains("superseded by a security fix"),
                "{}",
                finding.message
            );
        }

        #[tokio::test]
        async fn provider_ids_without_a_network_allowlist_reports_policy_violation() {
            let (cp, store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            // Declares a router provider id (requesting `ryuzi:provider-auth`) but
            // no network host, so `HostPolicy::for_installed_bundle` cannot grant
            // `allow_provider_auth`.
            let m = manifest(
                "acme-provider",
                "0.1.0",
                "^0.1.0",
                "provider-ids = [\"acme-free\"]\n",
            );
            let sha = write_bundle(
                root.path(),
                "acme-provider",
                "0.1.0",
                &m,
                b"component bytes",
                "0.1.0",
                &signing_key(),
                FIRST_PARTY,
            );
            seed_active(&store, "acme-provider", "0.1.0", &sha, FIRST_PARTY).await;

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), trusted_keys(), vec![]))
                    .await
                    .unwrap();
            let finding = findings
                .iter()
                .find(|f| f.kind == "policy-violation")
                .expect("provider-ids without a network host must report policy-violation");
            assert_eq!(finding.plugin_id, "acme-provider");
            assert_eq!(finding.severity, "warn");
            assert!(!finding.suggested_action.is_empty());
        }

        #[tokio::test]
        async fn structurally_invalid_manifest_reports_policy_violation() {
            let (cp, store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            // A network allowlist entry carrying a scheme is not a valid host, so
            // `PluginBundleManifest::validate` rejects it.
            let m = manifest(
                "acme-connector",
                "0.1.0",
                "^0.1.0",
                "[permissions]\nnetwork = [\"https://api.example.com\"]\n",
            );
            let sha = write_bundle(
                root.path(),
                "acme-connector",
                "0.1.0",
                &m,
                b"component bytes",
                "0.1.0",
                &signing_key(),
                FIRST_PARTY,
            );
            seed_active(&store, "acme-connector", "0.1.0", &sha, FIRST_PARTY).await;

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), trusted_keys(), vec![]))
                    .await
                    .unwrap();
            let finding = findings
                .iter()
                .find(|f| f.kind == "policy-violation" && f.severity == "error")
                .expect("an invalid manifest must report a policy-violation error");
            assert_eq!(finding.plugin_id, "acme-connector");
            assert!(
                findings.iter().all(|f| f.kind != "signature-invalid"),
                "a structural manifest defect must not be mislabeled as a signature defect: {findings:?}"
            );
        }

        #[tokio::test]
        async fn declared_oauth_profile_without_a_token_reports_unhealthy() {
            let (cp, store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            let m = manifest(
                "acme-connector",
                "0.1.0",
                "^0.1.0",
                "[[oauth]]\nid = \"main\"\n",
            );
            let sha = write_bundle(
                root.path(),
                "acme-connector",
                "0.1.0",
                &m,
                b"component bytes",
                "0.1.0",
                &signing_key(),
                FIRST_PARTY,
            );
            seed_active(&store, "acme-connector", "0.1.0", &sha, FIRST_PARTY).await;

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), trusted_keys(), vec![]))
                    .await
                    .unwrap();
            let finding = findings
                .iter()
                .find(|f| f.kind == "oauth-profile-unhealthy")
                .expect("a declared profile with no token must report oauth-profile-unhealthy");
            assert_eq!(finding.plugin_id, "acme-connector");
            assert_eq!(finding.severity, "warn");
            assert!(finding.message.contains("main"), "{}", finding.message);
        }

        #[tokio::test]
        async fn declared_oauth_profile_with_a_valid_token_is_healthy_and_secret_free() {
            crate::llm_router::secrets::use_test_key_file();
            let (cp, store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            let m = manifest(
                "acme-connector",
                "0.1.0",
                "^0.1.0",
                "[[oauth]]\nid = \"main\"\n",
            );
            let sha = write_bundle(
                root.path(),
                "acme-connector",
                "0.1.0",
                &m,
                b"component bytes",
                "0.1.0",
                &signing_key(),
                FIRST_PARTY,
            );
            seed_active(&store, "acme-connector", "0.1.0", &sha, FIRST_PARTY).await;
            store
                .upsert_plugin_oauth_profile_token(
                    "acme-connector",
                    "main",
                    &PluginOauthToken {
                        plugin_id: "acme-connector".into(),
                        access_token: "super-secret-access-token".into(),
                        refresh_token: None,
                        token_type: "Bearer".into(),
                        expires_at: Some(crate::paths::now_ms() + 3_600_000),
                        scopes: vec![],
                        reconnect_required: false,
                    },
                )
                .await
                .unwrap();

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), trusted_keys(), vec![]))
                    .await
                    .unwrap();
            assert!(
                findings.iter().all(|f| f.kind != "oauth-profile-unhealthy"),
                "a profile with a valid token must be healthy, got: {findings:?}"
            );
            for f in &findings {
                assert!(
                    !f.message.contains("super-secret-access-token"),
                    "no finding may leak a stored token: {}",
                    f.message
                );
            }
        }

        #[tokio::test]
        async fn declared_oauth_profile_with_an_expired_token_reports_unhealthy() {
            crate::llm_router::secrets::use_test_key_file();
            let (cp, store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            let m = manifest(
                "acme-connector",
                "0.1.0",
                "^0.1.0",
                "[[oauth]]\nid = \"main\"\n",
            );
            let sha = write_bundle(
                root.path(),
                "acme-connector",
                "0.1.0",
                &m,
                b"component bytes",
                "0.1.0",
                &signing_key(),
                FIRST_PARTY,
            );
            seed_active(&store, "acme-connector", "0.1.0", &sha, FIRST_PARTY).await;
            store
                .upsert_plugin_oauth_profile_token(
                    "acme-connector",
                    "main",
                    &PluginOauthToken {
                        plugin_id: "acme-connector".into(),
                        access_token: "expired-secret-token".into(),
                        refresh_token: None,
                        token_type: "Bearer".into(),
                        expires_at: Some(crate::paths::now_ms() - 3_600_000),
                        scopes: vec![],
                        reconnect_required: false,
                    },
                )
                .await
                .unwrap();

            let findings =
                plugin_doctor_with(&cp, &wasm_inputs(root.path(), trusted_keys(), vec![]))
                    .await
                    .unwrap();
            let finding = findings
                .iter()
                .find(|f| f.kind == "oauth-profile-unhealthy")
                .expect("an expired profile token must report oauth-profile-unhealthy");
            assert!(finding.message.contains("expired"), "{}", finding.message);
            for f in &findings {
                assert!(
                    !f.message.contains("expired-secret-token"),
                    "no finding may leak a stored token: {}",
                    f.message
                );
            }
        }

        #[tokio::test]
        async fn gateway_down_at_the_backoff_ceiling_reports_restart_exhausted() {
            let (cp, _store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            let findings = plugin_doctor_with(
                &cp,
                &wasm_inputs(
                    root.path(),
                    trusted_keys(),
                    vec![(
                        "discord".to_string(),
                        GatewayRestartHealth {
                            running: false,
                            restart_count: GATEWAY_BACKOFF_CEILING_RESTARTS,
                        },
                    )],
                ),
            )
            .await
            .unwrap();
            let finding = findings
                .iter()
                .find(|f| f.kind == "gateway-restart-exhausted")
                .expect("a down gateway at the backoff ceiling must report restart exhaustion");
            assert_eq!(finding.plugin_id, "discord");
            assert_eq!(finding.severity, "error");
            assert!(!finding.suggested_action.is_empty());
        }

        #[tokio::test]
        async fn running_or_recovering_gateway_reports_no_restart_finding() {
            let (cp, _store, _db) = cp_with_store().await;
            let root = tempfile::tempdir().unwrap();
            let findings = plugin_doctor_with(
                &cp,
                &wasm_inputs(
                    root.path(),
                    trusted_keys(),
                    vec![
                        // Running, however high the restart count: not exhausted.
                        (
                            "running".to_string(),
                            GatewayRestartHealth {
                                running: true,
                                restart_count: GATEWAY_BACKOFF_CEILING_RESTARTS + 5,
                            },
                        ),
                        // Down but still climbing below the ceiling: not yet exhausted.
                        (
                            "recovering".to_string(),
                            GatewayRestartHealth {
                                running: false,
                                restart_count: GATEWAY_BACKOFF_CEILING_RESTARTS - 1,
                            },
                        ),
                    ],
                ),
            )
            .await
            .unwrap();
            assert!(
                findings.iter().all(|f| f.kind != "gateway-restart-exhausted"),
                "a running or below-ceiling gateway must not report restart exhaustion, got: {findings:?}"
            );
        }

        /// Lowercase-hex a byte slice — used to assert key material never appears
        /// in a finding message.
        fn hex_of(bytes: &[u8]) -> String {
            bytes.iter().map(|b| format!("{b:02x}")).collect()
        }
    }
}
