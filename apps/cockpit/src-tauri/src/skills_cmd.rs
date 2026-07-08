use ryuzi_core::skills_install::{InstalledSkillInfo, InstalledSkillPack};

#[tauri::command]
#[specta::specta]
pub async fn list_skills() -> Result<Vec<InstalledSkillInfo>, String> {
    ryuzi_core::skills_install::list_installed_skills().map_err(|err| err.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn install_skill(source: String) -> Result<InstalledSkillPack, String> {
    ryuzi_core::skills_install::install_skill_source(&source)
        .await
        .map_err(|err| err.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn remove_skill(id: String) -> Result<(), String> {
    ryuzi_core::skills_install::remove_installed_skill(&id).map_err(|err| err.to_string())
}

#[tauri::command]
#[specta::specta]
pub async fn refresh_skill(id: String) -> Result<InstalledSkillPack, String> {
    ryuzi_core::skills_install::refresh_installed_skill(&id)
        .await
        .map_err(|err| err.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn install_skill_rejects_invalid_sources_with_string_errors() {
        let err = install_skill("not a valid source".to_string())
            .await
            .expect_err("invalid source should error");
        assert!(err.contains("unsupported skill source"));
    }

    #[tokio::test]
    async fn remove_skill_reports_unknown_ids_with_string_errors() {
        let err = remove_skill("missing-skill".to_string())
            .await
            .expect_err("missing install should error");
        assert!(err.contains("unknown installed skill"));
    }
}
