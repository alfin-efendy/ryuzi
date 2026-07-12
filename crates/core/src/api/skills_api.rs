//! Skills screen commands: list/install/remove/refresh git-backed native
//! skills and plugin-bundled skill packs. Moved verbatim (per the Move
//! Recipe) from `apps/cockpit/src-tauri/src/skills_cmd.rs`; that file keeps
//! its own copy until the proxy rewrite in Tasks 15-16.

use super::{ok, params, ApiError};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;

pub(crate) const HANDLES: &[&str] = &[
    "list_skills",
    "install_skill",
    "remove_skill",
    "refresh_skill",
];

#[derive(Deserialize)]
struct SourceP {
    source: String,
}
#[derive(Deserialize)]
struct IdP {
    id: String,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "list_skills" => ok(
            crate::skills_install::list_installed_skills().map_err(|message| ApiError {
                status: 500,
                message: message.to_string(),
            })?,
        ),
        // Gated single-call install: only completes immediately for the same
        // "curated AND doesn't run code" condition `begin_skill_install`'s
        // curated-immediate branch allows. Anything else (an arbitrary
        // source, or a manifest declaring `[[extension]]`) errors out
        // instead of installing — this RPC has no confirmation step of its
        // own, so it must never be a way to skip the two-phase trust gate.
        // Use `begin_skill_install`/`confirm_skill_install` for those.
        "install_skill" => {
            let a: SourceP = params(p)?;
            let pack = crate::skills_install::install_skill_source_gated(&a.source, cp.store())
                .await
                .map_err(|message| ApiError {
                    status: 500,
                    message: message.to_string(),
                })?;
            cp.mark_plugins_restart_required();
            ok(pack)
        }
        // Ledger-aware remove: `remove_installed_skill_recorded` also deletes
        // the pack's `plugin_installs`/`plugin_attach_status` rows so a
        // reinstall starts from a clean ledger state instead of resurrecting
        // stale trust/pin metadata. Marks a restart — an uninstall changes
        // what's on disk.
        "remove_skill" => {
            let a: IdP = params(p)?;
            crate::skills_install::remove_installed_skill_recorded(&a.id, cp.store())
                .await
                .map_err(|message| ApiError {
                    status: 500,
                    message: message.to_string(),
                })?;
            cp.mark_plugins_restart_required();
            ok(())
        }
        // Ledger-aware refresh: `refresh_installed_skill_recorded` keeps the
        // pack's `plugin_installs` fingerprint in sync so a bare refresh
        // doesn't leave the ledger's fingerprint stale (which would otherwise
        // false-positive a `LocalEdits` result on the next update). Marks a
        // restart, since a refresh reinstalls fresh content.
        "refresh_skill" => {
            let a: IdP = params(p)?;
            let pack = crate::skills_install::refresh_installed_skill_recorded(&a.id, cp.store())
                .await
                .map_err(|message| ApiError {
                    status: 500,
                    message: message.to_string(),
                })?;
            cp.mark_plugins_restart_required();
            ok(pack)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
}

#[cfg(test)]
mod tests {
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;

    #[tokio::test]
    async fn list_skills_dispatches_and_decodes_as_an_array() {
        let s = state().await;
        let out = dispatch(&s, "list_skills", json!({})).await.unwrap();
        assert!(
            out.is_array(),
            "expected list_skills to decode as an array, got: {out}"
        );
    }
}
