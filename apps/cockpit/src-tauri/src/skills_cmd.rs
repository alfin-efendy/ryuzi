use ryuzi_core::skills_install::{InstalledSkillInfo, InstalledSkillPack};
use ryuzi_core::ControlPlane;
use std::sync::Arc;
use tauri::State;

fn command_result<T>(result: anyhow::Result<T>) -> Result<T, String> {
    result.map_err(|err| err.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn list_skills() -> Result<Vec<InstalledSkillInfo>, String> {
    command_result(ryuzi_core::skills_install::list_installed_skills())
}

/// Remove a single-skill install. Ledger-aware: also deletes the pack's
/// `plugin_installs`/`plugin_attach_status` rows (via
/// `remove_installed_skill_recorded`) so a reinstall starts from a clean
/// ledger state instead of resurrecting stale trust/pin metadata, and marks
/// `plugins_restart_required` — an uninstall changes what's on disk.
#[tauri::command]
#[specta::specta]
pub async fn remove_skill(cp: State<'_, Arc<ControlPlane>>, id: String) -> Result<(), String> {
    let result = ryuzi_core::skills_install::remove_installed_skill_recorded(&id, cp.store())
        .await
        .map_err(|err| err.to_string());
    if result.is_ok() {
        cp.mark_plugins_restart_required();
    }
    result
}

/// Refresh a single-skill install. Ledger-aware: also keeps the pack's
/// `plugin_installs` fingerprint in sync (via
/// `refresh_installed_skill_recorded`) so a bare refresh doesn't leave the
/// ledger's fingerprint stale — which would otherwise false-positive a
/// `LocalEdits` result on the next update — and marks
/// `plugins_restart_required`, since a refresh reinstalls fresh content.
#[tauri::command]
#[specta::specta]
pub async fn refresh_skill(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
) -> Result<InstalledSkillPack, String> {
    let result = ryuzi_core::skills_install::refresh_installed_skill_recorded(&id, cp.store())
        .await
        .map_err(|err| err.to_string());
    if result.is_ok() {
        cp.mark_plugins_restart_required();
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn command_result_maps_invalid_source_errors_to_strings() {
        let err = command_result::<InstalledSkillPack>(Err(anyhow::anyhow!(
            "unsupported skill source: not a valid source"
        )))
        .expect_err("invalid source should error");
        assert!(err.contains("unsupported skill source"));
    }

    #[test]
    fn command_result_maps_unknown_skill_errors_to_strings() {
        let err = command_result::<()>(Err(anyhow::anyhow!(
            "unknown installed skill: missing-skill"
        )))
        .expect_err("missing install should error");
        assert!(err.contains("unknown installed skill"));
    }
}
