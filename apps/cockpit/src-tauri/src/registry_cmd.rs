//! Registry screen commands: live search against the official MCP registry
//! (registry.modelcontextprotocol.io). Install turns an entry into an Apps
//! row (npm package → `npx` stdio command; remote → HTTP URL).

use crate::error::CmdError;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::time::Duration;

type R<T> = Result<T, CmdError>;

const REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io/v0/servers";

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntry {
    /// Registry name, e.g. `io.github.owner/server`.
    pub id: String,
    pub name: String,
    pub desc: String,
    pub version: Option<String>,
    pub publisher: String,
    /// stdio (npm package) | http (remote)
    pub kind: String,
    /// npm identifier for stdio entries; URL for remotes.
    pub install_target: Option<String>,
    pub website: Option<String>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RegistryPage {
    pub entries: Vec<RegistryEntry>,
    pub next_cursor: Option<String>,
}

/// Map one registry item (tolerates both bare server.json and {server: …}).
pub fn map_entry(item: &serde_json::Value) -> Option<RegistryEntry> {
    let server = item.get("server").unwrap_or(item);
    let reg_name = server.get("name")?.as_str()?.to_string();
    let title = server
        .get("title")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| reg_name.split('/').next_back().unwrap_or(&reg_name).to_string());
    let publisher = reg_name.split('/').next().unwrap_or("").to_string();
    let desc = server
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string();
    let version = server.get("version").and_then(|v| v.as_str()).map(|s| s.to_string());
    let website = server.get("websiteUrl").and_then(|v| v.as_str()).map(|s| s.to_string());

    // Prefer an npm package (stdio), fall back to a remote URL.
    let npm = server
        .get("packages")
        .and_then(|p| p.as_array())
        .and_then(|pkgs| {
            pkgs.iter().find_map(|p| {
                let reg_type = p
                    .get("registryType")
                    .or_else(|| p.get("registry_type"))
                    .and_then(|r| r.as_str())?;
                (reg_type == "npm").then(|| p.get("identifier").and_then(|i| i.as_str()).map(|s| s.to_string()))?
            })
        });
    let remote = server
        .get("remotes")
        .and_then(|r| r.as_array())
        .and_then(|rs| rs.first())
        .and_then(|r| r.get("url"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string());

    let (kind, install_target) = match (npm, remote) {
        (Some(pkg), _) => ("stdio".to_string(), Some(pkg)),
        (None, Some(url)) => ("http".to_string(), Some(url)),
        (None, None) => ("unknown".to_string(), None),
    };

    Some(RegistryEntry {
        id: reg_name,
        name: title,
        desc,
        version,
        publisher,
        kind,
        install_target,
        website,
    })
}

pub fn map_page(v: &serde_json::Value) -> RegistryPage {
    let entries = v
        .get("servers")
        .and_then(|s| s.as_array())
        .map(|items| items.iter().filter_map(map_entry).collect())
        .unwrap_or_default();
    let next_cursor = v
        .pointer("/metadata/nextCursor")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    RegistryPage { entries, next_cursor }
}

#[tauri::command]
#[specta::specta]
pub async fn registry_search(query: Option<String>, cursor: Option<String>) -> R<RegistryPage> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|e| CmdError { message: e.to_string() })?;
    let mut req = client.get(REGISTRY_BASE).query(&[("limit", "30")]);
    if let Some(q) = query.as_deref().filter(|q| !q.trim().is_empty()) {
        req = req.query(&[("search", q.trim())]);
    }
    if let Some(c) = cursor.as_deref() {
        req = req.query(&[("cursor", c)]);
    }
    let resp = req.send().await.map_err(|e| CmdError {
        message: format!("registry unreachable: {e}"),
    })?;
    if !resp.status().is_success() {
        return Err(CmdError {
            message: format!("registry returned HTTP {}", resp.status()),
        });
    }
    let v: serde_json::Value = resp.json().await.map_err(|e| CmdError {
        message: format!("registry response invalid: {e}"),
    })?;
    Ok(map_page(&v))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_npm_package_and_remote_entries() {
        let v: serde_json::Value = serde_json::json!({
            "servers": [
                {
                    "name": "io.github.getsentry/sentry-mcp",
                    "title": "Sentry",
                    "description": "Query issues and stack traces.",
                    "version": "1.2.0",
                    "packages": [{ "registryType": "npm", "identifier": "@sentry/mcp-server" }]
                },
                {
                    "name": "ac.inference.sh/mcp",
                    "description": "Run AI apps.",
                    "version": "1.0.1",
                    "remotes": [{ "type": "streamable-http", "url": "https://sh.inference.ac" }]
                }
            ],
            "metadata": { "nextCursor": "abc:1.0", "count": 2 }
        });
        let page = map_page(&v);
        assert_eq!(page.entries.len(), 2);
        assert_eq!(page.next_cursor.as_deref(), Some("abc:1.0"));

        let sentry = &page.entries[0];
        assert_eq!(sentry.name, "Sentry");
        assert_eq!(sentry.kind, "stdio");
        assert_eq!(sentry.install_target.as_deref(), Some("@sentry/mcp-server"));
        assert_eq!(sentry.publisher, "io.github.getsentry");

        let remote = &page.entries[1];
        assert_eq!(remote.kind, "http");
        assert_eq!(remote.install_target.as_deref(), Some("https://sh.inference.ac"));
        // No title → falls back to the last path segment of the name.
        assert_eq!(remote.name, "mcp");
    }
}
