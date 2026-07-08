use ryuzi_core::skills_install::{InstalledSkillInfo, InstalledSkillPack};

fn command_result<T>(result: anyhow::Result<T>) -> Result<T, String> {
    result.map_err(|err| err.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn list_skills() -> Result<Vec<InstalledSkillInfo>, String> {
    command_result(ryuzi_core::skills_install::list_installed_skills())
}

#[tauri::command]
#[specta::specta]
pub async fn install_skill(source: String) -> Result<InstalledSkillPack, String> {
    command_result(ryuzi_core::skills_install::install_skill_source(&source).await)
}

#[tauri::command]
#[specta::specta]
pub async fn remove_skill(id: String) -> Result<(), String> {
    command_result(ryuzi_core::skills_install::remove_installed_skill(&id))
}

#[tauri::command]
#[specta::specta]
pub async fn refresh_skill(id: String) -> Result<InstalledSkillPack, String> {
    command_result(ryuzi_core::skills_install::refresh_installed_skill(&id).await)
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
