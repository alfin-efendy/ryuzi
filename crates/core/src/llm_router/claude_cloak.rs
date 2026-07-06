use crate::llm_router::connections::ConnectionData;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub type ToolNameMap = HashMap<String, String>;

const CLAUDE_VERSION: &str = "2.1.92";
const CC_ENTRYPOINT: &str = "sdk-cli";
pub const CLAUDE_TOOL_SUFFIX: &str = "_ide";
const PROVIDER_SPECIFIC_KEY: &str = "claudeCloaking";

const CC_DECOY_TOOL_NAMES: &[&str] = &[
    "Task",
    "TaskOutput",
    "TaskStop",
    "TaskCreate",
    "TaskGet",
    "TaskUpdate",
    "TaskList",
    "Bash",
    "Glob",
    "Grep",
    "Read",
    "Edit",
    "Write",
    "NotebookEdit",
    "WebFetch",
    "WebSearch",
    "AskUserQuestion",
    "Skill",
    "EnterPlanMode",
    "ExitPlanMode",
];

pub fn enabled(data: &ConnectionData) -> bool {
    data.provider_specific
        .as_ref()
        .and_then(|v| v.get(PROVIDER_SPECIFIC_KEY))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}

pub fn set_enabled(data: &mut ConnectionData, enabled: bool) {
    let mut obj = data
        .provider_specific
        .take()
        .and_then(|v| v.as_object().cloned())
        .unwrap_or_default();
    if enabled {
        obj.insert(PROVIDER_SPECIFIC_KEY.to_string(), Value::Bool(true));
    } else {
        obj.remove(PROVIDER_SPECIFIC_KEY);
    }
    data.provider_specific = if obj.is_empty() {
        None
    } else {
        Some(Value::Object(obj))
    };
}

pub fn apply_request_cloak(body: &mut Value, access_token: &str, session_id: &str) -> ToolNameMap {
    let map = cloak_tools(body);
    inject_billing_block(body);
    inject_user_id(body, access_token, session_id);
    map
}

pub fn tool_name_map_from_request(body: &Value) -> ToolNameMap {
    body.get("tools")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|tool| tool.get("type").is_none())
        .filter_map(|tool| tool.get("name").and_then(Value::as_str))
        .map(|name| (suffixed(name), name.to_string()))
        .collect()
}

pub fn tool_name_map_for(provider: &str, data: &ConnectionData, body: &Value) -> ToolNameMap {
    if provider == "anthropic-oauth" && enabled(data) {
        tool_name_map_from_request(body)
    } else {
        ToolNameMap::new()
    }
}

pub fn decloak_response(body: &mut Value, map: &ToolNameMap) {
    let Some(content) = body.get_mut("content").and_then(Value::as_array_mut) else {
        return;
    };
    for block in content {
        if block.get("type").and_then(Value::as_str) == Some("tool_use") {
            decloak_name_field(block, map);
        }
    }
}

pub fn decloak_event(event: &str, data: &mut Value, map: &ToolNameMap) {
    if event != "content_block_start" {
        return;
    }
    let Some(block) = data.get_mut("content_block") else {
        return;
    };
    if block.get("type").and_then(Value::as_str) == Some("tool_use") {
        decloak_name_field(block, map);
    }
}

pub fn spoof_headers(req: reqwest::RequestBuilder, session_id: &str) -> reqwest::RequestBuilder {
    req.header("x-stainless-runtime-version", "v24.14.0")
        .header("x-stainless-package-version", "0.80.0")
        .header("x-stainless-runtime", "node")
        .header("x-stainless-lang", "js")
        .header("x-stainless-arch", stainless_arch())
        .header("x-stainless-os", stainless_os())
        .header("x-stainless-timeout", "600")
        .header("x-claude-code-session-id", session_id)
}

fn cloak_tools(body: &mut Value) -> ToolNameMap {
    let mut map = ToolNameMap::new();
    let mut client_names = std::collections::HashSet::new();

    if let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) {
        let mut cloaked = Vec::with_capacity(tools.len() + CC_DECOY_TOOL_NAMES.len());
        for tool in std::mem::take(tools) {
            if tool.get("type").is_some() {
                cloaked.push(tool);
                continue;
            }
            let Some(name) = tool.get("name").and_then(Value::as_str) else {
                cloaked.push(tool);
                continue;
            };
            let original = name.to_string();
            let renamed = suffixed(&original);
            map.insert(renamed.clone(), original.clone());
            client_names.insert(original);
            let mut next = tool;
            next["name"] = json!(renamed);
            cloaked.push(next);
        }
        for name in CC_DECOY_TOOL_NAMES {
            cloaked.push(json!({
                "name": name,
                "description": "This tool is currently unavailable.",
                "input_schema": {"type": "object", "properties": {}}
            }));
        }
        *tools = cloaked;
    }

    if let Some(messages) = body.get_mut("messages").and_then(Value::as_array_mut) {
        for message in messages {
            let Some(content) = message.get_mut("content").and_then(Value::as_array_mut) else {
                continue;
            };
            for block in content {
                if block.get("type").and_then(Value::as_str) != Some("tool_use") {
                    continue;
                }
                if let Some(name) = block.get("name").and_then(Value::as_str) {
                    block["name"] = json!(suffixed(name));
                }
            }
        }
    }

    if body
        .get("tool_choice")
        .and_then(|v| v.get("type"))
        .and_then(Value::as_str)
        == Some("tool")
    {
        if let Some(name) = body
            .get("tool_choice")
            .and_then(|v| v.get("name"))
            .and_then(Value::as_str)
        {
            if client_names.contains(name) {
                body["tool_choice"]["name"] = json!(suffixed(name));
            }
        }
    }

    map
}

fn inject_billing_block(body: &mut Value) {
    let billing_text = billing_header(body);
    let billing_block = json!({"type": "text", "text": billing_text});
    let current = body.get("system").cloned().unwrap_or(Value::Null);
    body["system"] = match current {
        Value::Array(mut arr) => {
            if !arr
                .first()
                .and_then(|v| v.get("text"))
                .and_then(Value::as_str)
                .map(|s| s.starts_with("x-anthropic-billing-header:"))
                .unwrap_or(false)
            {
                arr.insert(0, billing_block);
            }
            Value::Array(arr)
        }
        Value::String(s) => json!([billing_block, {"type": "text", "text": s}]),
        _ => json!([billing_block]),
    };
}

fn inject_user_id(body: &mut Value, access_token: &str, session_id: &str) {
    if body
        .get("metadata")
        .and_then(|v| v.get("user_id"))
        .is_some()
    {
        return;
    }
    if !body.get("metadata").is_some_and(Value::is_object) {
        body["metadata"] = json!({});
    }
    body["metadata"]["user_id"] = json!(fake_user_id(access_token, session_id));
}

fn decloak_name_field(value: &mut Value, map: &ToolNameMap) {
    let Some(name) = value.get("name").and_then(Value::as_str) else {
        return;
    };
    if let Some(original) = map.get(name) {
        value["name"] = json!(original);
    }
}

fn suffixed(name: &str) -> String {
    format!("{name}{CLAUDE_TOOL_SUFFIX}")
}

fn billing_header(body: &Value) -> String {
    let content = serde_json::to_string(body).unwrap_or_default();
    let cch = hex_sha256(&content).chars().take(5).collect::<String>();
    let build = hex_sha256(&format!("build:{content}"))
        .chars()
        .take(3)
        .collect::<String>();
    format!(
        "x-anthropic-billing-header: cc_version={CLAUDE_VERSION}.{build}; cc_entrypoint={CC_ENTRYPOINT}; cch={cch};"
    )
}

fn fake_user_id(access_token: &str, session_id: &str) -> String {
    let device_id = hex_sha256(&format!("device:{access_token}"));
    let account_uuid = derive_uuid(&format!("account:{access_token}"));
    format!(
        r#"{{"device_id":"{device_id}","account_uuid":"{account_uuid}","session_id":"{session_id}"}}"#
    )
}

fn derive_uuid(seed: &str) -> String {
    let h = hex_sha256(seed);
    let variant = u8::from_str_radix(&h[16..17], 16).unwrap_or(0);
    let variant = format!("{:x}", (variant & 0x3) | 0x8);
    format!(
        "{}-{}-4{}-{}{}-{}",
        &h[0..8],
        &h[8..12],
        &h[13..16],
        variant,
        &h[17..20],
        &h[20..32]
    )
}

fn hex_sha256(input: &str) -> String {
    let digest = Sha256::digest(input.as_bytes());
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

fn stainless_os() -> &'static str {
    match std::env::consts::OS {
        "macos" => "MacOS",
        "windows" => "Windows",
        "linux" => "Linux",
        "freebsd" => "FreeBSD",
        _ => "Other",
    }
}

fn stainless_arch() -> &'static str {
    match std::env::consts::ARCH {
        "x86_64" => "x64",
        "aarch64" => "arm64",
        "x86" => "x86",
        _ => "other",
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    #[test]
    fn full_cloak_rewrites_tools_metadata_and_billing_block() {
        let mut body = json!({
            "system": [{"type": "text", "text": "You are Claude Code"}],
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "tu_1", "name": "lookup", "input": {"q": "x"}}
                ]}
            ],
            "tools": [
                {"name": "lookup", "description": "Lookup data", "input_schema": {"type": "object"}},
                {"type": "web_search_20250305", "name": "web_search"}
            ],
            "tool_choice": {"type": "tool", "name": "lookup"}
        });

        let map = super::apply_request_cloak(&mut body, "sk-ant-oat-test", "session-1");

        assert_eq!(map.get("lookup_ide").map(String::as_str), Some("lookup"));
        assert_eq!(body["tools"][0]["name"], "lookup_ide");
        assert_eq!(body["tools"][1]["name"], "web_search");
        assert!(body["tools"]
            .as_array()
            .unwrap()
            .iter()
            .any(|tool| tool["name"] == "Bash"));
        assert_eq!(body["messages"][0]["content"][0]["name"], "lookup_ide");
        assert_eq!(body["tool_choice"]["name"], "lookup_ide");
        assert!(body["system"][0]["text"]
            .as_str()
            .unwrap()
            .starts_with("x-anthropic-billing-header: cc_version=2.1.92."));
        let user_id = body["metadata"]["user_id"].as_str().unwrap();
        assert!(user_id.contains("\"device_id\""));
        assert!(user_id.contains("\"session_id\":\"session-1\""));
    }

    #[test]
    fn decloak_restores_tool_names_in_message_and_stream_event() {
        let map = super::ToolNameMap::from([("lookup_ide".to_string(), "lookup".to_string())]);
        let mut response = json!({
            "content": [
                {"type": "tool_use", "id": "tu_1", "name": "lookup_ide", "input": {}}
            ]
        });
        super::decloak_response(&mut response, &map);
        assert_eq!(response["content"][0]["name"], "lookup");

        let mut event = json!({
            "type": "content_block_start",
            "index": 0,
            "content_block": {"type": "tool_use", "id": "tu_2", "name": "lookup_ide", "input": {}}
        });
        super::decloak_event("content_block_start", &mut event, &map);
        assert_eq!(event["content_block"]["name"], "lookup");
    }
}
