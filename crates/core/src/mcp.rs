//! Apps (MCP servers) domain: persisted server definitions with per-tool
//! permissions and per-agent access, a real stdio JSON-RPC probe
//! (initialize → tools/list), and the bridge that attaches enabled servers to
//! agent sessions through `SessionCtx.mcp_servers`.

use crate::domain::{McpServerSpec, McpTransport};
use crate::store::Store;
use rusqlite::{params, OptionalExtension};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

#[derive(Debug, Clone, PartialEq)]
pub struct McpServerRow {
    /// Slug id — also the MCP server name agents see (`mcp__<id>__<tool>`).
    pub id: String,
    pub name: String,
    pub kind: String,
    pub color: String,
    pub description: String,
    /// stdio | http
    pub transport: String,
    pub command: Option<String>,
    pub args: Vec<String>,
    pub env: Vec<(String, String)>,
    pub url: Option<String>,
    /// global | select
    pub scope: String,
    pub scope_gateways: Vec<String>,
    pub version: Option<String>,
    pub publisher: Option<String>,
    /// connected | error | unknown
    pub status: String,
    pub status_detail: Option<String>,
    pub auth_kind: String,
    pub auth_detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct McpToolRow {
    pub name: String,
    pub description: String,
    /// allow | ask | deny
    pub perm: String,
}

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

const SERVER_COLS: &str = "id,name,kind,color,description,transport,command,args,env,url,scope,scope_gateways,version,publisher,status,status_detail,auth_kind,auth_detail";

fn server_from(r: &rusqlite::Row) -> rusqlite::Result<McpServerRow> {
    let args: String = r.get(7)?;
    let env: String = r.get(8)?;
    let scope_gateways: String = r.get(11)?;
    Ok(McpServerRow {
        id: r.get(0)?,
        name: r.get(1)?,
        kind: r.get(2)?,
        color: r.get(3)?,
        description: r.get(4)?,
        transport: r.get(5)?,
        command: r.get(6)?,
        args: serde_json::from_str(&args).unwrap_or_default(),
        env: serde_json::from_str::<std::collections::BTreeMap<String, String>>(&env)
            .map(|m| m.into_iter().collect())
            .unwrap_or_default(),
        url: r.get(9)?,
        scope: r.get(10)?,
        scope_gateways: serde_json::from_str(&scope_gateways).unwrap_or_default(),
        version: r.get(12)?,
        publisher: r.get(13)?,
        status: r.get(14)?,
        status_detail: r.get(15)?,
        auth_kind: r.get(16)?,
        auth_detail: r.get(17)?,
    })
}

pub async fn list_servers(store: &Store) -> anyhow::Result<Vec<McpServerRow>> {
    store
        .with_conn(|c| {
            let mut stmt = c.prepare(&format!(
                "SELECT {SERVER_COLS} FROM mcp_servers ORDER BY created_at"
            ))?;
            let rows = stmt
                .query_map([], server_from)?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

pub async fn get_server(store: &Store, id: &str) -> anyhow::Result<Option<McpServerRow>> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                &format!("SELECT {SERVER_COLS} FROM mcp_servers WHERE id=?1"),
                params![id],
                server_from,
            )
            .optional()
        })
        .await
}

pub async fn upsert_server(store: &Store, row: McpServerRow) -> anyhow::Result<()> {
    let args = serde_json::to_string(&row.args)?;
    let env_map: std::collections::BTreeMap<_, _> = row.env.iter().cloned().collect();
    let env = serde_json::to_string(&env_map)?;
    let scope_gateways = serde_json::to_string(&row.scope_gateways)?;
    let now = crate::paths::now_ms();
    store
        .with_conn(move |c| {
            c.execute(
                &format!(
                    "INSERT INTO mcp_servers({SERVER_COLS},created_at) \
                     VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19) \
                     ON CONFLICT(id) DO UPDATE SET \
                       name=excluded.name, kind=excluded.kind, color=excluded.color, \
                       description=excluded.description, transport=excluded.transport, \
                       command=excluded.command, args=excluded.args, env=excluded.env, \
                       url=excluded.url, scope=excluded.scope, scope_gateways=excluded.scope_gateways, \
                       version=excluded.version, publisher=excluded.publisher, status=excluded.status, \
                       status_detail=excluded.status_detail, auth_kind=excluded.auth_kind, \
                       auth_detail=excluded.auth_detail"
                ),
                params![
                    row.id, row.name, row.kind, row.color, row.description, row.transport,
                    row.command, args, env, row.url, row.scope, scope_gateways,
                    row.version, row.publisher, row.status, row.status_detail,
                    row.auth_kind, row.auth_detail, now
                ],
            )
            .map(|_| ())
        })
        .await
}

pub async fn remove_server(store: &Store, id: &str) -> anyhow::Result<()> {
    let id = id.to_string();
    store
        .with_conn(move |c| {
            c.execute("DELETE FROM mcp_tools WHERE server_id=?1", params![id])?;
            c.execute(
                "DELETE FROM mcp_agent_access WHERE server_id=?1",
                params![id],
            )?;
            c.execute("DELETE FROM mcp_servers WHERE id=?1", params![id])
                .map(|_| ())
        })
        .await
}

pub async fn list_tools(store: &Store, server_id: &str) -> anyhow::Result<Vec<McpToolRow>> {
    let server_id = server_id.to_string();
    store
        .with_conn(move |c| {
            let mut stmt = c.prepare(
                "SELECT name, description, perm FROM mcp_tools WHERE server_id=?1 ORDER BY name",
            )?;
            let rows = stmt
                .query_map(params![server_id], |r| {
                    Ok(McpToolRow {
                        name: r.get(0)?,
                        description: r.get(1)?,
                        perm: r.get(2)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

/// Replace the discovered tool list, preserving perms for tools that survive.
pub async fn replace_tools(
    store: &Store,
    server_id: &str,
    tools: Vec<(String, String)>,
) -> anyhow::Result<()> {
    let existing = list_tools(store, server_id).await?;
    let server_id = server_id.to_string();
    store
        .with_conn(move |c| {
            c.execute("DELETE FROM mcp_tools WHERE server_id=?1", params![server_id])?;
            for (name, desc) in tools {
                let perm = existing
                    .iter()
                    .find(|t| t.name == name)
                    .map(|t| t.perm.clone())
                    .unwrap_or_else(|| "ask".to_string());
                c.execute(
                    "INSERT INTO mcp_tools(server_id, name, description, perm) VALUES (?1,?2,?3,?4)",
                    params![server_id, name, desc, perm],
                )?;
            }
            Ok(())
        })
        .await
}

pub async fn set_tool_perm(
    store: &Store,
    server_id: &str,
    tool: &str,
    perm: &str,
) -> anyhow::Result<()> {
    let server_id = server_id.to_string();
    let tool = tool.to_string();
    let perm = perm.to_string();
    store
        .with_conn(move |c| {
            c.execute(
                "UPDATE mcp_tools SET perm=?3 WHERE server_id=?1 AND name=?2",
                params![server_id, tool, perm],
            )
            .map(|_| ())
        })
        .await
}

pub async fn agent_access(store: &Store, server_id: &str) -> anyhow::Result<Vec<(String, bool)>> {
    let server_id = server_id.to_string();
    store
        .with_conn(move |c| {
            let mut stmt =
                c.prepare("SELECT agent_id, allowed FROM mcp_agent_access WHERE server_id=?1")?;
            let rows = stmt
                .query_map(params![server_id], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .await
}

pub async fn set_agent_access(
    store: &Store,
    server_id: &str,
    agent_id: &str,
    allowed: bool,
) -> anyhow::Result<()> {
    let server_id = server_id.to_string();
    let agent_id = agent_id.to_string();
    store
        .with_conn(move |c| {
            c.execute(
                "INSERT INTO mcp_agent_access(server_id, agent_id, allowed) VALUES (?1,?2,?3) \
                 ON CONFLICT(server_id, agent_id) DO UPDATE SET allowed=excluded.allowed",
                params![server_id, agent_id, allowed as i64],
            )
            .map(|_| ())
        })
        .await
}

/// Whether `agent_id` may use `server_id` (unset → allowed by default).
pub async fn agent_allowed(store: &Store, server_id: &str, agent_id: &str) -> anyhow::Result<bool> {
    Ok(agent_access(store, server_id)
        .await?
        .into_iter()
        .find(|(a, _)| a == agent_id)
        .map(|(_, allowed)| allowed)
        .unwrap_or(true))
}

// ---------------------------------------------------------------------------
// Session attachment
// ---------------------------------------------------------------------------

/// The MCP servers to attach to a new local session for `agent_id`: enabled
/// scope (global or explicitly including `local`) and agent access allowed.
pub async fn servers_for_session(
    store: &Store,
    agent_id: &str,
) -> anyhow::Result<Vec<McpServerSpec>> {
    let mut out = Vec::new();
    for row in list_servers(store).await? {
        let in_scope = row.scope == "global" || row.scope_gateways.iter().any(|g| g == "local");
        if !in_scope || !agent_allowed(store, &row.id, agent_id).await? {
            continue;
        }
        let transport = match row.transport.as_str() {
            "http" => match &row.url {
                Some(url) => McpTransport::Http {
                    url: url.clone(),
                    headers: vec![],
                },
                None => continue,
            },
            _ => match &row.command {
                Some(command) => McpTransport::Stdio {
                    command: command.clone(),
                    args: row.args.clone(),
                    env: row.env.clone(),
                },
                None => continue,
            },
        };
        out.push(McpServerSpec {
            name: row.id.clone(),
            transport,
        });
    }
    Ok(out)
}

/// Convert an engine spec into the ACP wire type for `session/new`.
pub fn to_acp(spec: &McpServerSpec) -> agent_client_protocol_schema::v1::McpServer {
    use agent_client_protocol_schema::v1 as acp;
    match &spec.transport {
        McpTransport::Stdio { command, args, env } => acp::McpServer::Stdio(
            acp::McpServerStdio::new(spec.name.clone(), command.clone())
                .args(args.clone())
                .env(
                    env.iter()
                        .map(|(k, v)| acp::EnvVariable::new(k.clone(), v.clone()))
                        .collect(),
                ),
        ),
        McpTransport::Http { url, headers } => acp::McpServer::Http(
            acp::McpServerHttp::new(spec.name.clone(), url.clone()).headers(
                headers
                    .iter()
                    .map(|(k, v)| acp::HttpHeader::new(k.clone(), v.clone()))
                    .collect(),
            ),
        ),
    }
}

// ---------------------------------------------------------------------------
// Tool-permission bridge (`mcp__<server>__<tool>` names)
// ---------------------------------------------------------------------------

/// Split a Claude-style MCP tool name into (server, tool).
pub fn mcp_tool_parts(name: &str) -> Option<(&str, &str)> {
    let rest = name.strip_prefix("mcp__")?;
    let (server, tool) = rest.split_once("__")?;
    if server.is_empty() || tool.is_empty() {
        return None;
    }
    Some((server, tool))
}

/// The persisted permission for an MCP tool title, if it is one.
pub async fn tool_perm_for_title(store: &Store, title: &str) -> Option<String> {
    let (server, tool) = mcp_tool_parts(title)?;
    let server = server.to_string();
    let tool = tool.to_string();
    store
        .with_conn(move |c| {
            c.query_row(
                "SELECT perm FROM mcp_tools WHERE server_id=?1 AND name=?2",
                params![server, tool],
                |r| r.get::<_, String>(0),
            )
            .optional()
        })
        .await
        .ok()
        .flatten()
}

// ---------------------------------------------------------------------------
// Stdio probe (newline-delimited JSON-RPC)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Default)]
pub struct ProbeResult {
    pub ok: bool,
    pub server_version: Option<String>,
    pub tools: Vec<(String, String)>,
    pub error: Option<String>,
}

/// Extract the JSON-RPC response with `id` from a line, if it is one.
pub fn parse_response_line(line: &str, id: i64) -> Option<serde_json::Value> {
    let v: serde_json::Value = serde_json::from_str(line.trim()).ok()?;
    (v.get("id").and_then(|i| i.as_i64()) == Some(id)).then_some(v)
}

/// Pull `(name, description)` pairs out of a `tools/list` result.
pub fn parse_tools_result(v: &serde_json::Value) -> Vec<(String, String)> {
    v.pointer("/result/tools")
        .and_then(|t| t.as_array())
        .map(|tools| {
            tools
                .iter()
                .filter_map(|t| {
                    Some((
                        t.get("name")?.as_str()?.to_string(),
                        t.get("description")
                            .and_then(|d| d.as_str())
                            .unwrap_or("")
                            .to_string(),
                    ))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// Spawn a stdio MCP server, run initialize → tools/list, and tear it down.
pub async fn probe_stdio(command: &str, args: &[String], env: &[(String, String)]) -> ProbeResult {
    match tokio::time::timeout(
        Duration::from_secs(25),
        probe_stdio_inner(command, args, env),
    )
    .await
    {
        Ok(result) => result,
        Err(_) => ProbeResult {
            ok: false,
            error: Some("probe timed out after 25s".into()),
            ..Default::default()
        },
    }
}

async fn probe_stdio_inner(
    command: &str,
    args: &[String],
    env: &[(String, String)],
) -> ProbeResult {
    let fail = |error: String| ProbeResult {
        ok: false,
        error: Some(error),
        ..Default::default()
    };

    // .cmd shims (npx on Windows) must run through cmd.exe.
    let is_shim = cfg!(windows)
        && (command.to_ascii_lowercase().ends_with(".cmd")
            || command.to_ascii_lowercase().ends_with(".bat")
            || !command.contains(['/', '\\', '.']));
    let mut cmd = if is_shim {
        let mut c = tokio::process::Command::new("cmd");
        c.arg("/C").arg(command).args(args);
        c
    } else {
        let mut c = tokio::process::Command::new(command);
        c.args(args);
        c
    };
    for (k, v) in env {
        cmd.env(k, v);
    }
    cmd.stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    {
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }

    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return fail(format!("failed to spawn: {e}")),
    };
    let mut stdin = child.stdin.take().expect("piped stdin");
    let stdout = child.stdout.take().expect("piped stdout");
    let mut lines = BufReader::new(stdout).lines();

    let init = serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {
            "protocolVersion": "2025-06-18",
            "capabilities": {},
            "clientInfo": { "name": "ryuzi-cockpit", "version": env!("CARGO_PKG_VERSION") }
        }
    });
    if let Err(e) = stdin.write_all(format!("{init}\n").as_bytes()).await {
        return fail(format!("failed to write initialize: {e}"));
    }

    let init_resp = loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Some(v) = parse_response_line(&line, 1) {
                    break v;
                }
            }
            Ok(None) => return fail("server closed stdout during initialize".into()),
            Err(e) => return fail(format!("read error: {e}")),
        }
    };
    if let Some(err) = init_resp.get("error") {
        return fail(format!("initialize error: {err}"));
    }
    let server_version = init_resp
        .pointer("/result/serverInfo/version")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    let initialized =
        serde_json::json!({ "jsonrpc": "2.0", "method": "notifications/initialized" });
    let _ = stdin.write_all(format!("{initialized}\n").as_bytes()).await;

    let tools_req = serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" });
    if let Err(e) = stdin.write_all(format!("{tools_req}\n").as_bytes()).await {
        return fail(format!("failed to write tools/list: {e}"));
    }
    let tools_resp = loop {
        match lines.next_line().await {
            Ok(Some(line)) => {
                if let Some(v) = parse_response_line(&line, 2) {
                    break v;
                }
            }
            Ok(None) => return fail("server closed stdout during tools/list".into()),
            Err(e) => return fail(format!("read error: {e}")),
        }
    };

    let tools = parse_tools_result(&tools_resp);
    let _ = child.kill().await;
    ProbeResult {
        ok: true,
        server_version,
        tools,
        error: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_mcp_tool_titles() {
        assert_eq!(
            mcp_tool_parts("mcp__github__create_pr"),
            Some(("github", "create_pr"))
        );
        assert_eq!(
            mcp_tool_parts("mcp__pg__query__nested"),
            Some(("pg", "query__nested"))
        );
        assert_eq!(mcp_tool_parts("Bash"), None);
        assert_eq!(mcp_tool_parts("mcp__justserver"), None);
    }

    #[test]
    fn parses_jsonrpc_frames_and_tools() {
        assert!(parse_response_line("{\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}", 1).is_some());
        assert!(parse_response_line("{\"jsonrpc\":\"2.0\",\"id\":2,\"result\":{}}", 1).is_none());
        assert!(parse_response_line("not json", 1).is_none());

        let v: serde_json::Value = serde_json::from_str(
            "{\"id\":2,\"result\":{\"tools\":[{\"name\":\"query\",\"description\":\"Run SQL\"},{\"name\":\"bare\"}]}}",
        )
        .unwrap();
        assert_eq!(
            parse_tools_result(&v),
            vec![
                ("query".to_string(), "Run SQL".to_string()),
                ("bare".to_string(), String::new())
            ]
        );
    }

    #[tokio::test]
    async fn server_rows_tools_and_access_roundtrip() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();

        upsert_server(
            &store,
            McpServerRow {
                id: "github".into(),
                name: "GitHub".into(),
                kind: "MCP server".into(),
                color: "#24292F".into(),
                description: "PRs and issues".into(),
                transport: "stdio".into(),
                command: Some("npx".into()),
                args: vec!["-y".into(), "@modelcontextprotocol/server-github".into()],
                env: vec![("GITHUB_TOKEN".into(), "x".into())],
                url: None,
                scope: "global".into(),
                scope_gateways: vec![],
                version: Some("1.0.0".into()),
                publisher: Some("github".into()),
                status: "unknown".into(),
                status_detail: None,
                auth_kind: "env".into(),
                auth_detail: Some("GITHUB_TOKEN".into()),
            },
        )
        .await
        .unwrap();

        let rows = list_servers(&store).await.unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].env,
            vec![("GITHUB_TOKEN".to_string(), "x".to_string())]
        );

        // Tool discovery keeps perms across refreshes.
        replace_tools(
            &store,
            "github",
            vec![("create_pr".into(), "Open a PR".into())],
        )
        .await
        .unwrap();
        set_tool_perm(&store, "github", "create_pr", "deny")
            .await
            .unwrap();
        replace_tools(
            &store,
            "github",
            vec![
                ("create_pr".into(), "Open a PR".into()),
                ("list_issues".into(), "List issues".into()),
            ],
        )
        .await
        .unwrap();
        let tools = list_tools(&store, "github").await.unwrap();
        assert_eq!(
            tools.iter().find(|t| t.name == "create_pr").unwrap().perm,
            "deny"
        );
        assert_eq!(
            tools.iter().find(|t| t.name == "list_issues").unwrap().perm,
            "ask"
        );

        // Perm lookup by mcp title.
        assert_eq!(
            tool_perm_for_title(&store, "mcp__github__create_pr")
                .await
                .as_deref(),
            Some("deny")
        );
        assert_eq!(tool_perm_for_title(&store, "Bash").await, None);

        // Agent access defaults to allowed until set.
        assert!(agent_allowed(&store, "github", "claude").await.unwrap());
        set_agent_access(&store, "github", "claude", false)
            .await
            .unwrap();
        assert!(!agent_allowed(&store, "github", "claude").await.unwrap());

        // Session attachment honors scope + access.
        let specs = servers_for_session(&store, "claude").await.unwrap();
        assert!(
            specs.is_empty(),
            "denied agent access must exclude the server"
        );
        set_agent_access(&store, "github", "claude", true)
            .await
            .unwrap();
        let specs = servers_for_session(&store, "claude").await.unwrap();
        assert_eq!(specs.len(), 1);
        assert_eq!(specs[0].name, "github");

        remove_server(&store, "github").await.unwrap();
        assert!(list_servers(&store).await.unwrap().is_empty());
        assert!(list_tools(&store, "github").await.unwrap().is_empty());
    }
}
