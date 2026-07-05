//! Tauri commands exposing the native runtime's agents, slash commands, and
//! per-session todos to Cockpit.

use crate::error::CmdError;
use ryuzi_core::harness::native::agents::AgentRegistry;
use ryuzi_core::harness::native::commands::CommandRegistry;
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::path::Path;
use std::sync::Arc;
use tauri::State;

type R<T> = Result<T, CmdError>;

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct AgentInfo {
    pub name: String,
    pub description: String,
    pub mode: String,
    pub builtin: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct CommandInfo {
    pub name: String,
    pub description: String,
    pub agent: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct TodoItem {
    pub content: String,
    pub status: String,
}

async fn project_workdir(cp: &ControlPlane, project_id: &str) -> Result<String, CmdError> {
    let projects = cp.list_projects().await?;
    projects
        .into_iter()
        .find(|p| p.project_id == project_id)
        .map(|p| p.workdir)
        .ok_or_else(|| CmdError::from(anyhow::anyhow!("unknown project {project_id}")))
}

/// The agents available for a project (built-ins plus discovered custom agents).
#[tauri::command]
#[specta::specta]
pub async fn native_agents(
    cp: State<'_, Arc<ControlPlane>>,
    project_id: String,
) -> R<Vec<AgentInfo>> {
    let workdir = project_workdir(&cp, &project_id).await?;
    let reg = AgentRegistry::load(Path::new(&workdir));
    Ok(reg
        .all()
        .into_iter()
        .map(|a| AgentInfo {
            name: a.name,
            description: a.description,
            mode: a.mode.as_str().to_string(),
            builtin: a.builtin,
        })
        .collect())
}

/// The slash commands available for a project.
#[tauri::command]
#[specta::specta]
pub async fn native_commands(
    cp: State<'_, Arc<ControlPlane>>,
    project_id: String,
) -> R<Vec<CommandInfo>> {
    let workdir = project_workdir(&cp, &project_id).await?;
    let reg = CommandRegistry::load(Path::new(&workdir));
    Ok(reg
        .all()
        .into_iter()
        .map(|c| CommandInfo {
            name: c.name,
            description: c.description,
            agent: c.agent,
        })
        .collect())
}

/// A session's current native todo list.
#[tauri::command]
#[specta::specta]
pub async fn session_todos(
    cp: State<'_, Arc<ControlPlane>>,
    session_pk: String,
) -> R<Vec<TodoItem>> {
    let rows = cp.store().list_todos(&session_pk).await?;
    Ok(rows
        .into_iter()
        .map(|(content, status)| TodoItem { content, status })
        .collect())
}
