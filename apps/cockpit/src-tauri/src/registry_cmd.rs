//! Registry screen commands: live search against the official MCP registry
//! (registry.modelcontextprotocol.io). Install turns an entry into an Apps
//! row (npm package → `npx` stdio command; remote → HTTP URL).

use crate::error::CmdError;
use serde::{Deserialize, Serialize};
use specta::Type;
use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::time::Duration;

type R<T> = Result<T, CmdError>;
type ServerListItem = serde_json::Value;

const REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io/v0/servers";

#[derive(Debug, Clone, Serialize, Deserialize, Type)]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntryVersion {
    pub version: String,
    pub install_target: Option<String>,
    pub website: Option<String>,
    pub is_latest: bool,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RegistryEntry {
    /// Registry name, e.g. `io.github.owner/server`.
    pub id: String,
    pub name: String,
    pub desc: String,
    pub version: String,
    pub publisher: Option<String>,
    /// stdio (npm package) | http (remote)
    pub kind: String,
    /// npm identifier for stdio entries; URL for remotes.
    pub install_target: Option<String>,
    pub website: Option<String>,
    pub versions: Vec<RegistryEntryVersion>,
}

#[derive(Serialize, Deserialize, Type, Clone)]
#[serde(rename_all = "camelCase")]
pub struct RegistryPage {
    pub entries: Vec<RegistryEntry>,
    pub next_cursor: Option<String>,
}

fn server_object(item: &ServerListItem) -> &serde_json::Value {
    item.get("server").unwrap_or(item)
}

fn normalize_name(name: &str) -> String {
    name.trim().to_lowercase()
}

fn entry_name(item: &ServerListItem) -> String {
    let server = server_object(item);
    let reg_name = server.get("name").and_then(|n| n.as_str()).unwrap_or("");
    server
        .get("title")
        .and_then(|t| t.as_str())
        .map(|s| s.to_string())
        .unwrap_or_else(|| {
            reg_name
                .split('/')
                .next_back()
                .unwrap_or(reg_name)
                .to_string()
        })
}

fn entry_desc(item: &ServerListItem) -> String {
    server_object(item)
        .get("description")
        .and_then(|d| d.as_str())
        .unwrap_or("")
        .to_string()
}

fn entry_version(item: &ServerListItem) -> String {
    server_object(item)
        .get("version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

fn entry_publisher(item: &ServerListItem) -> Option<String> {
    server_object(item)
        .get("name")
        .and_then(|name| name.as_str())
        .and_then(|name| {
            name.split('/')
                .next()
                .filter(|v| !v.is_empty())
                .map(str::to_string)
        })
}

fn entry_install_target(item: &ServerListItem) -> Option<String> {
    let server = server_object(item);
    let npm = server
        .get("packages")
        .and_then(|p| p.as_array())
        .and_then(|pkgs| {
            pkgs.iter().find_map(|p| {
                let reg_type = p
                    .get("registryType")
                    .or_else(|| p.get("registry_type"))
                    .and_then(|r| r.as_str())?;
                (reg_type == "npm").then(|| {
                    p.get("identifier")
                        .and_then(|i| i.as_str())
                        .map(|s| s.to_string())
                })?
            })
        });
    let remote = server
        .get("remotes")
        .and_then(|r| r.as_array())
        .and_then(|rs| rs.first())
        .and_then(|r| r.get("url"))
        .and_then(|u| u.as_str())
        .map(|s| s.to_string());

    match (npm, remote) {
        (Some(pkg), _) => Some(pkg),
        (None, Some(url)) => Some(url),
        (None, None) => None,
    }
}

fn entry_kind(item: &ServerListItem) -> String {
    let has_npm = entry_install_target(item).is_some_and(|_| {
        server_object(item)
            .get("packages")
            .and_then(|packages| packages.as_array())
            .is_some_and(|pkgs| {
                pkgs.iter().any(|pkg| {
                    pkg.get("registryType")
                        .or_else(|| pkg.get("registry_type"))
                        .and_then(|ty| ty.as_str())
                        == Some("npm")
                })
            })
    });
    if has_npm {
        "stdio".to_string()
    } else if server_object(item)
        .get("remotes")
        .and_then(|r| r.as_array())
        .is_some()
    {
        "http".to_string()
    } else {
        "unknown".to_string()
    }
}

fn entry_website(item: &ServerListItem) -> Option<String> {
    server_object(item)
        .get("websiteUrl")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string())
}

fn official_server_meta(item: &ServerListItem) -> Option<&serde_json::Value> {
    server_object(item)
        .get("_meta")
        .or_else(|| item.get("_meta"))
        .and_then(|meta| meta.get("io.modelcontextprotocol.registry/official"))
}

fn split_to_version_parts(version: &str) -> Vec<&str> {
    version.split('.').collect()
}

fn compare_version_segments(left: &str, right: &str) -> Ordering {
    match (left.parse::<u64>(), right.parse::<u64>()) {
        (Ok(left), Ok(right)) => right.cmp(&left),
        (Ok(_), Err(_)) => Ordering::Less,
        (Err(_), Ok(_)) => Ordering::Greater,
        (Err(_), Err(_)) => right.cmp(left),
    }
}

fn compare_version_desc(left: &str, right: &str) -> Ordering {
    let left_parts = split_to_version_parts(left);
    let right_parts = split_to_version_parts(right);
    let max = left_parts.len().max(right_parts.len());

    for idx in 0..max {
        let l = left_parts.get(idx).copied().unwrap_or("0");
        let r = right_parts.get(idx).copied().unwrap_or("0");
        let ord = compare_version_segments(l, r);
        if !ord.is_eq() {
            return ord;
        }
    }

    Ordering::Equal
}

fn compare_versions(a: &RegistryEntryVersion, b: &RegistryEntryVersion) -> Ordering {
    if a.is_latest != b.is_latest {
        return if a.is_latest {
            Ordering::Less
        } else {
            Ordering::Greater
        };
    }
    compare_version_desc(&a.version, &b.version)
}

/// Map one registry item into a version DTO.
pub fn map_version(item: &ServerListItem) -> RegistryEntryVersion {
    RegistryEntryVersion {
        version: entry_version(item),
        install_target: entry_install_target(item),
        website: entry_website(item),
        is_latest: official_server_meta(item)
            .and_then(|official| official.get("isLatest"))
            .and_then(|v| v.as_bool())
            .unwrap_or(false),
    }
}

/// Deterministic grouping key for a live registry list item.
/// Plugin identity is `server.name`, with display-name fallback only when missing.
pub fn entry_key(item: &ServerListItem) -> String {
    server_object(item)
        .get("name")
        .and_then(|name| name.as_str())
        .map(normalize_name)
        .unwrap_or_else(|| normalize_name(&entry_name(item)))
}

/// Group server versions into installables with semantic-descending versions.
pub fn group_entries(items: Vec<ServerListItem>) -> Vec<RegistryEntry> {
    let mut grouped: BTreeMap<String, Vec<ServerListItem>> = BTreeMap::new();
    for item in items {
        let key = entry_key(&item);
        grouped.entry(key).or_default().push(item);
    }

    let mut entries = Vec::with_capacity(grouped.len());
    for (key, items) in grouped {
        let mut versions: Vec<(ServerListItem, RegistryEntryVersion)> = items
            .into_iter()
            .map(|item| {
                let version = map_version(&item);
                (item, version)
            })
            .collect();
        if versions.is_empty() {
            continue;
        }
        versions.sort_by(|(_, a), (_, b)| compare_versions(a, b));

        let (latest_item, latest_version) = versions[0].clone();
        let version_payload: Vec<RegistryEntryVersion> = versions
            .iter()
            .map(|(_, version)| version)
            .cloned()
            .collect();
        let kind = entry_kind(&latest_item);

        entries.push(RegistryEntry {
            id: key,
            name: entry_name(&latest_item),
            desc: entry_desc(&latest_item),
            version: latest_version.version.clone(),
            publisher: entry_publisher(&latest_item),
            kind,
            install_target: latest_version.install_target.clone(),
            website: latest_version.website.clone(),
            versions: version_payload,
        });
    }

    entries
}

/// Map one registry response item to a raw server object.
fn map_entry(item: &serde_json::Value) -> Option<ServerListItem> {
    let server = server_object(item);
    server.as_object()?;
    Some(server.clone())
}

pub fn map_page(v: &serde_json::Value) -> RegistryPage {
    let entries = v
        .get("servers")
        .and_then(|s| s.as_array())
        .map(|items| items.iter().filter_map(map_entry).collect::<Vec<_>>())
        .map(group_entries)
        .unwrap_or_default();
    let next_cursor = v
        .pointer("/metadata/nextCursor")
        .and_then(|c| c.as_str())
        .map(|s| s.to_string());
    RegistryPage {
        entries,
        next_cursor,
    }
}

#[tauri::command]
#[specta::specta]
pub async fn registry_search(query: Option<String>, cursor: Option<String>) -> R<RegistryPage> {
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(12))
        .build()
        .map_err(|e| CmdError {
            message: e.to_string(),
        })?;
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
        let v: serde_json::Value = serde_json::json!( {
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

        let sentry = page
            .entries
            .iter()
            .find(|entry| entry.id == "io.github.getsentry/sentry-mcp")
            .expect("sentry entry");
        assert_eq!(sentry.name, "Sentry");
        assert_eq!(sentry.kind, "stdio");
        assert_eq!(sentry.install_target.as_deref(), Some("@sentry/mcp-server"));
        assert_eq!(sentry.publisher.as_deref(), Some("io.github.getsentry"));
        assert_eq!(sentry.version, "1.2.0");

        let remote = page
            .entries
            .iter()
            .find(|entry| entry.id == "ac.inference.sh/mcp")
            .expect("remote entry");
        assert_eq!(remote.kind, "http");
        assert_eq!(
            remote.install_target.as_deref(),
            Some("https://sh.inference.ac")
        );
        // No title → falls back to the last path segment of the name.
        assert_eq!(remote.name, "mcp");
    }

    #[test]
    fn groups_multiple_versions_for_plugin_identity() {
        let first: ServerListItem = serde_json::json!({
            "name": "io.github.example/legacy",
            "title": "Example MCP",
            "description": "Example MCP server.",
            "version": "1.0.0",
            "packages": [{ "registryType": "npm", "identifier": "example-server@1.0.0" }],
            "_meta": { "io.modelcontextprotocol.registry/official": { "isLatest": false }}
        });

        let second: ServerListItem = serde_json::json!({
            "name": "io.github.example/legacy",
            "title": "Example MCP",
            "description": "Example MCP server.",
            "version": "1.1.0",
            "packages": [{ "registryType": "npm", "identifier": "example-server@1.1.0" }],
            "_meta": { "io.modelcontextprotocol.registry/official": { "isLatest": true }}
        });

        let entries = group_entries(vec![first, second]);
        assert_eq!(entries.len(), 1);

        let entry = &entries[0];
        assert_eq!(entry.id, "io.github.example/legacy");
        assert_eq!(entry.version, "1.1.0");
        assert_eq!(
            entry.install_target.as_deref(),
            Some("example-server@1.1.0")
        );
        assert_eq!(entry.versions.len(), 2);
        assert_eq!(entry.versions[0].version, "1.1.0");
        assert!(entry.versions[0].is_latest);
        assert_eq!(entry.versions[1].version, "1.0.0");
        assert!(!entry.versions[1].is_latest);
    }

    #[test]
    fn groups_by_server_name_not_official_metadata_id() {
        let same_identity_first: ServerListItem = serde_json::json!({
            "name": "io.github.example/legacy",
            "title": "Example MCP",
            "description": "Example MCP server.",
            "version": "1.0.0",
            "packages": [{ "registryType": "npm", "identifier": "example-server@1.0.0" }],
            "_meta": {
                "io.modelcontextprotocol.registry/official": {
                    "isLatest": false,
                    "id": "id-legacy-1.0.0",
                },
            }
        });

        let same_identity_second: ServerListItem = serde_json::json!({
            "name": "io.github.example/legacy",
            "title": "Example MCP",
            "description": "Example MCP server.",
            "version": "1.1.0",
            "packages": [{ "registryType": "npm", "identifier": "example-server@1.1.0" }],
            "_meta": {
                "io.modelcontextprotocol.registry/official": {
                    "isLatest": true,
                    "id": "id-legacy-1.1.0",
                },
            }
        });

        let entries = group_entries(vec![same_identity_first, same_identity_second]);
        assert_eq!(entries.len(), 1);
        let entry = &entries[0];
        assert_eq!(entry.id, "io.github.example/legacy");
        assert_eq!(entry.name, "Example MCP");
        assert_eq!(entry.versions.len(), 2);
        assert_eq!(entry.versions[0].version, "1.1.0");
        assert!(entry.versions[0].is_latest);
        assert_eq!(entry.versions[1].version, "1.0.0");
        assert!(!entry.versions[1].is_latest);
        assert_eq!(entry.version, "1.1.0");
    }
}
