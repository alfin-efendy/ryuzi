//! Apps screen commands. MCP server definitions persist in SQLite; `probe_app`
//! does a real stdio handshake (initialize → tools/list) or an HTTP
//! reachability check; enabled servers attach to agent sessions for real via
//! `SessionCtx.mcp_servers`. Moved verbatim (per the Move Recipe) from
//! `apps/cockpit/src-tauri/src/apps_cmd.rs`; that file keeps its own copy
//! until the proxy rewrite in Tasks 15-16.

use super::{ok, params, ApiError};
use crate::api::types::*;
use crate::control::ControlPlane;
use crate::mcp::{self, McpServerRow};
use crate::serve::ApiState;
use serde::Deserialize;
use serde_json::Value;
use std::time::Duration;

pub(crate) const HANDLES: &[&str] = &[
    "list_apps",
    "add_app",
    "remove_app",
    "probe_app",
    "update_app_scope",
    "set_app_tool_perm",
    "toggle_app_agent",
];

#[derive(Deserialize)]
struct InputP {
    input: AddAppInput,
}
#[derive(Deserialize)]
struct IdP {
    id: String,
}
#[derive(Deserialize)]
struct UpdateScopeP {
    id: String,
    scope: String,
    scope_gateways: Vec<String>,
}
#[derive(Deserialize)]
struct ToolPermP {
    id: String,
    tool: String,
    perm: String,
}
#[derive(Deserialize)]
struct ToggleAgentP {
    id: String,
    agent_id: String,
    allowed: bool,
}

pub(crate) async fn dispatch(state: &ApiState, method: &str, p: Value) -> Result<Value, ApiError> {
    let cp = &state.cp;
    match method {
        "list_apps" => ok(assemble(cp).await?),
        "add_app" => {
            let a: InputP = params(p)?;
            ok(add_app(state, a.input).await?)
        }
        "remove_app" => {
            let a: IdP = params(p)?;
            mcp::remove_server(cp.store(), &a.id).await?;
            ok(assemble(cp).await?)
        }
        "probe_app" => {
            let a: IdP = params(p)?;
            ok(probe_app(state, a.id).await?)
        }
        "update_app_scope" => {
            let a: UpdateScopeP = params(p)?;
            ok(update_app_scope(state, a.id, a.scope, a.scope_gateways).await?)
        }
        "set_app_tool_perm" => {
            let a: ToolPermP = params(p)?;
            mcp::set_tool_perm(cp.store(), &a.id, &a.tool, &a.perm).await?;
            ok(assemble(cp).await?)
        }
        "toggle_app_agent" => {
            let a: ToggleAgentP = params(p)?;
            mcp::set_agent_access(cp.store(), &a.id, &a.agent_id, a.allowed).await?;
            ok(assemble(cp).await?)
        }
        _ => Err(ApiError::not_found(format!("unknown method: {method}"))),
    }
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
    let agent_ids: Vec<&str> = crate::runtimes::CATALOG.iter().map(|d| d.id).collect();
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
        let mut agent_access = Vec::new();
        for aid in &agent_ids {
            agent_access.push(AgentAccessInfo {
                agent_id: aid.to_string(),
                allowed: mcp::agent_allowed(cp.store(), &row.id, aid).await?,
            });
        }
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

async fn add_app(state: &ApiState, input: AddAppInput) -> Result<Vec<AppInfo>, ApiError> {
    let cp = &state.cp;
    let id = input.id.clone().unwrap_or_else(|| slugify(&input.name));
    if id.is_empty() {
        return Err(ApiError::bad_request("app needs a name"));
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
    probe_and_persist(cp, &id).await?;
    Ok(assemble(cp).await?)
}

async fn probe_app(state: &ApiState, id: String) -> Result<Vec<AppInfo>, ApiError> {
    let cp = &state.cp;
    probe_and_persist(cp, &id).await?;
    Ok(assemble(cp).await?)
}

async fn update_app_scope(
    state: &ApiState,
    id: String,
    scope: String,
    scope_gateways: Vec<String>,
) -> Result<Vec<AppInfo>, ApiError> {
    let cp = &state.cp;
    let mut row = mcp::get_server(cp.store(), &id)
        .await?
        .ok_or_else(|| ApiError::not_found(format!("unknown app: {id}")))?;
    row.scope = scope;
    row.scope_gateways = scope_gateways;
    mcp::upsert_server(cp.store(), row).await?;
    Ok(assemble(cp).await?)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{dispatch, tests_support::state};
    use serde_json::json;

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

    #[tokio::test]
    async fn list_apps_returns_empty_on_fresh_store_via_rpc() {
        let s = state().await;
        let out = dispatch(&s, "list_apps", json!({})).await.unwrap();
        assert_eq!(out, json!([]));
    }
}
