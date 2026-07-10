//! Apps screen commands. MCP server definitions persist in SQLite; `probe_app`
//! does a real stdio handshake (initialize → tools/list) or an HTTP
//! reachability check; enabled servers attach to agent sessions for real via
//! `SessionCtx.mcp_servers`.

use crate::error::CmdError;
use ryuzi_core::mcp::{self, McpServerRow};
use ryuzi_core::ControlPlane;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::sync::Arc;
use std::time::Duration;
use tauri::State;

type R<T> = Result<T, CmdError>;

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct ToolInfo {
    pub name: String,
    pub desc: String,
    pub perm: String,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AgentAccessInfo {
    pub agent_id: String,
    pub allowed: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub initial: String,
    pub color: String,
    pub desc: String,
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub url: Option<String>,
    pub scope: String,
    pub scope_gateways: Vec<String>,
    pub status: String,
    pub status_detail: Option<String>,
    pub version: Option<String>,
    pub publisher: Option<String>,
    pub auth_kind: String,
    pub auth_detail: Option<String>,
    pub tools: Vec<ToolInfo>,
    pub agent_access: Vec<AgentAccessInfo>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct AddAppInput {
    pub id: Option<String>,
    pub name: String,
    pub description: String,
    pub kind: Option<String>,
    /// stdio | http
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    /// KEY=VALUE pairs.
    pub env: Vec<String>,
    pub url: Option<String>,
    pub version: Option<String>,
    pub publisher: Option<String>,
    pub color: Option<String>,
}

fn slugify(name: &str) -> String {
    let slug: String = name
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    slug.trim_matches('-').replace("--", "-")
}

fn initial_of(name: &str) -> String {
    name.chars()
        .next()
        .map(|c| c.to_uppercase().to_string())
        .unwrap_or_else(|| "?".into())
}

/// Parse `KEY=VALUE` lines into pairs. Lines without a `=` (including blank
/// lines) are dropped, keys and values are whitespace-trimmed, and the value
/// keeps any further `=` characters.
fn parse_env_lines(lines: &[String]) -> Vec<(String, String)> {
    lines
        .iter()
        .filter_map(|line| {
            line.split_once('=')
                .map(|(k, v)| (k.trim().to_string(), v.trim().to_string()))
        })
        .collect()
}

/// Classify how the app authenticates: any env var means "env", with the
/// detail listing variable names only (never values); no env vars means
/// "none" with no detail.
fn derive_auth(env: &[(String, String)]) -> (&'static str, Option<String>) {
    let auth_kind = if env.is_empty() { "none" } else { "env" };
    let auth_detail = (!env.is_empty()).then(|| {
        env.iter()
            .map(|(k, _)| k.clone())
            .collect::<Vec<_>>()
            .join(", ")
    });
    (auth_kind, auth_detail)
}

async fn assemble(cp: &ControlPlane) -> anyhow::Result<Vec<AppInfo>> {
    let mut out = Vec::new();
    for row in mcp::list_servers(cp.store()).await? {
        let tools = mcp::list_tools(cp.store(), &row.id)
            .await?
            .into_iter()
            .map(|t| ToolInfo {
                name: t.name,
                desc: t.description,
                perm: t.perm,
            })
            .collect();
        // Native-only: "native" is the only agent id.
        let agent_access = vec![AgentAccessInfo {
            agent_id: "native".to_string(),
            allowed: mcp::agent_allowed(cp.store(), &row.id, "native").await?,
        }];
        out.push(AppInfo {
            initial: initial_of(&row.name),
            id: row.id,
            name: row.name,
            kind: row.kind,
            color: row.color,
            desc: row.description,
            transport: row.transport,
            command: row.command,
            args: row.args,
            url: row.url,
            scope: row.scope,
            scope_gateways: row.scope_gateways,
            status: row.status,
            status_detail: row.status_detail,
            version: row.version,
            publisher: row.publisher,
            auth_kind: row.auth_kind,
            auth_detail: row.auth_detail,
            tools,
            agent_access,
        });
    }
    Ok(out)
}

/// Probe one server and persist status/version/tools.
async fn probe_and_persist(cp: &ControlPlane, id: &str) -> anyhow::Result<()> {
    let Some(mut row) = mcp::get_server(cp.store(), id).await? else {
        anyhow::bail!("unknown app: {id}");
    };
    if row.transport == "http" {
        let url = row.url.clone().unwrap_or_default();
        let ok = match reqwest::Client::builder().timeout(Duration::from_secs(8)).build() {
            Ok(client) => client
                .post(&url)
                .header("Content-Type", "application/json")
                .header("Accept", "application/json, text/event-stream")
                .body(
                    serde_json::json!({
                        "jsonrpc": "2.0", "id": 1, "method": "initialize",
                        "params": {
                            "protocolVersion": "2025-06-18",
                            "capabilities": {},
                            "clientInfo": { "name": "ryuzi-cockpit", "version": env!("CARGO_PKG_VERSION") }
                        }
                    })
                    .to_string(),
                )
                .send()
                .await
                .map(|r| r.status().is_success())
                .unwrap_or(false),
            Err(_) => false,
        };
        row.status = if ok { "connected" } else { "error" }.into();
        row.status_detail = (!ok).then(|| "HTTP initialize failed — check the URL".to_string());
        mcp::upsert_server(cp.store(), row).await?;
        return Ok(());
    }

    let command = row.command.clone().unwrap_or_default();
    let result = mcp::probe_stdio(&command, &row.args, &row.env).await;
    row.status = if result.ok { "connected" } else { "error" }.into();
    row.status_detail = result.error.clone();
    if let Some(v) = &result.server_version {
        row.version = Some(v.clone());
    }
    let tools = result.tools.clone();
    mcp::upsert_server(cp.store(), row).await?;
    if result.ok {
        mcp::replace_tools(cp.store(), id, tools).await?;
    }
    Ok(())
}

#[tauri::command]
#[specta::specta]
pub async fn list_apps(cp: State<'_, Arc<ControlPlane>>) -> R<Vec<AppInfo>> {
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn add_app(cp: State<'_, Arc<ControlPlane>>, input: AddAppInput) -> R<Vec<AppInfo>> {
    let id = input.id.clone().unwrap_or_else(|| slugify(&input.name));
    if id.is_empty() {
        return Err(CmdError {
            message: "app needs a name".into(),
        });
    }
    let env = parse_env_lines(&input.env);
    let (auth_kind, auth_detail) = derive_auth(&env);
    mcp::upsert_server(
        cp.store(),
        McpServerRow {
            id: id.clone(),
            name: input.name,
            kind: input.kind.unwrap_or_else(|| "MCP server".into()),
            color: input.color.unwrap_or_else(|| "#8B8B8B".into()),
            description: input.description,
            transport: input.transport,
            command: input.command,
            args: input.args,
            env,
            url: input.url,
            scope: "global".into(),
            scope_gateways: vec![],
            version: input.version,
            publisher: input.publisher,
            status: "unknown".into(),
            status_detail: None,
            auth_kind: auth_kind.into(),
            auth_detail,
        },
    )
    .await?;
    // Real handshake right away so the card shows a true status + tool list.
    probe_and_persist(&cp, &id).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn remove_app(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<Vec<AppInfo>> {
    mcp::remove_server(cp.store(), &id).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn probe_app(cp: State<'_, Arc<ControlPlane>>, id: String) -> R<Vec<AppInfo>> {
    probe_and_persist(&cp, &id).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn update_app_scope(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    scope: String,
    scope_gateways: Vec<String>,
) -> R<Vec<AppInfo>> {
    let mut row = mcp::get_server(cp.store(), &id)
        .await?
        .ok_or_else(|| CmdError {
            message: format!("unknown app: {id}"),
        })?;
    row.scope = scope;
    row.scope_gateways = scope_gateways;
    mcp::upsert_server(cp.store(), row).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn set_app_tool_perm(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    tool: String,
    perm: String,
) -> R<Vec<AppInfo>> {
    mcp::set_tool_perm(cp.store(), &id, &tool, &perm).await?;
    Ok(assemble(&cp).await?)
}

#[tauri::command]
#[specta::specta]
pub async fn toggle_app_agent(
    cp: State<'_, Arc<ControlPlane>>,
    id: String,
    agent_id: String,
    allowed: bool,
) -> R<Vec<AppInfo>> {
    mcp::set_agent_access(cp.store(), &id, &agent_id, allowed).await?;
    Ok(assemble(&cp).await?)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lines(items: &[&str]) -> Vec<String> {
        items.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn env_lines_skip_blanks_and_lines_without_equals() {
        let parsed = parse_env_lines(&lines(&["FOO=bar", "", "no-separator", "BAZ=qux"]));
        assert_eq!(
            parsed,
            vec![
                ("FOO".to_string(), "bar".to_string()),
                ("BAZ".to_string(), "qux".to_string()),
            ]
        );
    }

    #[test]
    fn env_lines_trim_key_and_value() {
        let parsed = parse_env_lines(&lines(&[" API_KEY = secret value "]));
        assert_eq!(
            parsed,
            vec![("API_KEY".to_string(), "secret value".to_string())]
        );
    }

    #[test]
    fn env_lines_split_on_first_equals_only() {
        let parsed = parse_env_lines(&lines(&["TOKEN=abc=def"]));
        assert_eq!(parsed, vec![("TOKEN".to_string(), "abc=def".to_string())]);
    }

    #[test]
    fn no_env_means_no_auth() {
        assert_eq!(derive_auth(&[]), ("none", None));
    }

    #[test]
    fn env_auth_lists_variable_names_only() {
        let env = vec![
            ("API_KEY".to_string(), "secret".to_string()),
            ("ORG".to_string(), "acme".to_string()),
        ];
        assert_eq!(derive_auth(&env), ("env", Some("API_KEY, ORG".to_string())));
    }

    #[test]
    fn slugify_lowercases_and_dashes_non_alphanumerics() {
        assert_eq!(slugify("My App!"), "my-app");
        assert_eq!(slugify("sentry"), "sentry");
        assert_eq!(slugify("a  b"), "a-b");
        assert_eq!(slugify(""), "");
    }

    #[test]
    fn initial_is_uppercased_first_char_or_placeholder() {
        assert_eq!(initial_of("ryuzi"), "R");
        assert_eq!(initial_of("42nd"), "4");
        assert_eq!(initial_of(""), "?");
    }
}
