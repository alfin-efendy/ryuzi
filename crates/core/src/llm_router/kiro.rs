//! Kiro (AWS CodeWhisperer) request translators: Anthropic/OpenAI client
//! bodies → `generateAssistantResponse` payloads.
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors) —
//! open-sse/translator/request/{claude-to-kiro,openai-to-kiro}.js.
//!
//! MVP scope (correctness-critical subset): strict role alternation (merge
//! consecutive same-role turns with "\n\n"), system-prompt folding + the
//! always-present `"[Context: Current time is <RFC3339>]"` timestamp prefix
//! on `currentMessage` content (so the model has current-date context), tool
//! specs, tool results, `toolUses` in history, the last-user-message →
//! `currentMessage` split, and the two 400-guards (flatten tool blocks to
//! text when the client sent no tools; drop/salvage orphaned tool results
//! whose id has no matching `toolUses`).
//!
//! The assembled `currentMessage` content is, joined with "\n\n":
//! `[Context: Current time is <ts>]` + (optional folded system text) + user
//! content. The prefix goes ONLY on `currentMessage.content`, never on
//! history items.
//!
//! Deferred (documented, not built — see task-6 brief): image blocks; the
//! `-thinking` / `-agentic` synthetic model-suffix variants + thinking-budget
//! injection + agentic system prompt (only the `[Context:...]` line is added
//! now); `inferenceConfig.temperature`/`topP`. `maxTokens` =
//! `body.max_tokens || 32000` (anthropic) / hardcoded `32000` (openai) —
//! matches 9router's known behavior, not "fixed" here.

use crate::llm_router::aws_stream;
use crate::llm_router::connections::{self, ConnectionData};
use serde_json::{json, Value};

#[derive(Debug, Clone, Copy, PartialEq)]
enum Role {
    User,
    Assistant,
}

// ---------------------------------------------------------------------------
// Shared: profile ARN attachment (brief Step 3)
// ---------------------------------------------------------------------------

/// Account-bound auth (api_key/idc/external_idp) uses its OWN `kiro_profile_arn`
/// or omits the field entirely — NEVER the shared default (which belongs to a
/// different account and would 403 "bearer token invalid"). Non-account-bound
/// (builder-id/OAuth/social) auth falls back to the shared default ARN.
fn attach_profile_arn(payload: &mut Value, data: &ConnectionData) {
    let auth_method = connections::kiro_auth_method(data);
    let arn = if connections::is_account_bound(&auth_method) {
        connections::kiro_profile_arn(data)
    } else {
        Some(
            connections::kiro_profile_arn(data)
                .unwrap_or_else(|| connections::default_profile_arn(&auth_method).to_string()),
        )
    };
    if let Some(arn) = arn.filter(|s| !s.trim().is_empty()) {
        payload["profileArn"] = Value::String(arn);
    }
}

// ---------------------------------------------------------------------------
// Shared: tool call / tool result text rendering (guards)
// ---------------------------------------------------------------------------

/// Render a tool_use/tool_call as a readable text line — used by both 400
/// guards (flatten-when-no-tools) across both routes.
fn tool_use_bracket_text(name: &str, input: &Value) -> String {
    let arg_str = match input {
        Value::String(s) => s.clone(),
        Value::Null => "{}".to_string(),
        other => serde_json::to_string(other).unwrap_or_else(|_| "{}".to_string()),
    };
    let name = if name.is_empty() { "unknown" } else { name };
    format!("[Tool call: {name}({arg_str})]")
}

/// Claude tool_result block content → bracketed text (flatten guard, claude route).
fn claude_tool_result_bracket_text(content: &Value) -> String {
    let text = match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|c| match c {
                Value::String(s) => s.clone(),
                other => other["text"].as_str().unwrap_or("").to_string(),
            })
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    };
    format!("[Tool result: {text}]")
}

/// OpenAI tool-result content → bracketed text (flatten guard, openai route).
fn openai_tool_result_bracket_text(content: &Value) -> String {
    let text = match content {
        Value::Array(items) => items
            .iter()
            .map(|c| match c {
                Value::String(s) => s.clone(),
                other => other["text"].as_str().unwrap_or("").to_string(),
            })
            .collect::<Vec<_>>()
            .join("\n"),
        Value::String(s) => s.clone(),
        _ => String::new(),
    };
    format!("[Tool result: {text}]")
}

/// Claude tool_result block content → plain text for the STRUCTURED
/// `toolResults[].content[].text` field (not bracket-wrapped).
fn claude_tool_result_struct_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(items) => {
            let joined = items
                .iter()
                .filter(|c| c["type"] == "text")
                .filter_map(|c| c["text"].as_str())
                .collect::<Vec<_>>()
                .join("\n");
            if joined.is_empty() {
                serde_json::to_string(content).unwrap_or_default()
            } else {
                joined
            }
        }
        Value::Null => String::new(),
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

/// OpenAI tool_result block content → plain text for the STRUCTURED
/// `toolResults[].content[].text` field.
fn openai_tool_result_struct_text(content: &Value) -> String {
    match content {
        Value::Array(items) => items
            .iter()
            .map(|c| c["text"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        Value::String(s) => s.clone(),
        _ => String::new(),
    }
}

fn safe_json_parse(s: &str) -> Value {
    serde_json::from_str(s).unwrap_or_else(|_| json!({}))
}

// ---------------------------------------------------------------------------
// Shared: tool spec normalization
// ---------------------------------------------------------------------------

/// Empty schema → `{type:object,properties:{},required:[]}`; else keep the
/// schema, defaulting `required` to `[]` only when absent/null.
fn normalize_tool_schema(schema: &Value) -> Value {
    match schema.as_object() {
        Some(m) if !m.is_empty() => {
            let mut m = m.clone();
            if !matches!(m.get("required"), Some(v) if !v.is_null()) {
                m.insert("required".to_string(), json!([]));
            }
            Value::Object(m)
        }
        _ => json!({"type": "object", "properties": {}, "required": []}),
    }
}

fn tool_spec(name: &str, description: &str, schema: &Value) -> Value {
    let description = if description.trim().is_empty() {
        format!("Tool: {name}")
    } else {
        description.to_string()
    };
    json!({
        "toolSpecification": {
            "name": name,
            "description": description,
            "inputSchema": { "json": normalize_tool_schema(schema) }
        }
    })
}

/// Anthropic tool shape: `{name, description, input_schema}`.
fn build_tool_specs_claude(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let name = t["name"].as_str().unwrap_or("");
            let description = t["description"].as_str().unwrap_or("");
            tool_spec(name, description, &t["input_schema"])
        })
        .collect()
}

/// OpenAI tool shape: `{function:{name,description,parameters}}` (also
/// tolerates the Anthropic-ish flat `{name,description,input_schema}` shape,
/// matching 9router's fallback chain).
fn build_tool_specs_openai(tools: &[Value]) -> Vec<Value> {
    tools
        .iter()
        .map(|t| {
            let name = t["function"]["name"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| t["name"].as_str())
                .unwrap_or("");
            let description = t["function"]["description"]
                .as_str()
                .filter(|s| !s.is_empty())
                .or_else(|| t["description"].as_str().filter(|s| !s.is_empty()))
                .unwrap_or("");
            let schema = if !t["function"]["parameters"].is_null() {
                &t["function"]["parameters"]
            } else if !t["parameters"].is_null() {
                &t["parameters"]
            } else {
                &t["input_schema"]
            };
            tool_spec(name, description, schema)
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Shared: flush / pop / merge / reconcile (history assembly)
// ---------------------------------------------------------------------------

/// Flush the pending same-role buffer into a history entry: empty pending
/// user content becomes the literal `"continue"`; empty assistant content
/// becomes `"..."` — Kiro rejects empty turns.
fn flush_pending(
    role: Role,
    history: &mut Vec<Value>,
    pending_user: &mut Vec<String>,
    pending_assistant: &mut Vec<String>,
    pending_results: &mut Vec<Value>,
    model: &str,
) {
    match role {
        Role::User => {
            let joined = pending_user.join("\n\n");
            let trimmed = joined.trim();
            let content = if trimmed.is_empty() {
                "continue"
            } else {
                trimmed
            };
            let mut uim = serde_json::Map::new();
            uim.insert("content".into(), json!(content));
            uim.insert("modelId".into(), json!(model));
            if !pending_results.is_empty() {
                uim.insert(
                    "userInputMessageContext".into(),
                    json!({ "toolResults": std::mem::take(pending_results) }),
                );
            }
            history.push(json!({ "userInputMessage": Value::Object(uim) }));
            pending_user.clear();
        }
        Role::Assistant => {
            let joined = pending_assistant.join("\n\n");
            let trimmed = joined.trim();
            let content = if trimmed.is_empty() { "..." } else { trimmed };
            history.push(json!({ "assistantResponseMessage": { "content": content } }));
            pending_assistant.clear();
        }
    }
}

/// Pop the LAST userInputMessage out of `history` (search from the end,
/// skipping trailing assistant turns) to become `currentMessage`.
fn pop_current_message(history: &mut Vec<Value>) -> Option<Value> {
    for i in (0..history.len()).rev() {
        if history[i].get("userInputMessage").is_some() {
            return Some(history.remove(i));
        }
    }
    None
}

/// Merge adjacent `userInputMessage` history entries (Kiro requires strict
/// user/assistant alternation). Tools are never present on history entries
/// at this point (only `currentMessage` ever carries `tools` — see
/// `attach_tools_to_current`), so only `toolResults` needs concatenating.
fn merge_consecutive_user_turns(history: Vec<Value>) -> Vec<Value> {
    let mut merged: Vec<Value> = Vec::new();
    for current in history {
        let is_user = current.get("userInputMessage").is_some();
        let prev_is_user = merged
            .last()
            .map(|p| p.get("userInputMessage").is_some())
            .unwrap_or(false);
        if is_user && prev_is_user {
            let cur_content = current["userInputMessage"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            let cur_results = current["userInputMessage"]["userInputMessageContext"]["toolResults"]
                .as_array()
                .cloned()
                .unwrap_or_default();
            let prev = merged.last_mut().expect("prev_is_user implies non-empty");
            let prev_content = prev["userInputMessage"]["content"].as_str().unwrap_or("");
            prev["userInputMessage"]["content"] = json!(format!("{prev_content}\n\n{cur_content}"));
            if !cur_results.is_empty() {
                let mut combined = prev["userInputMessage"]["userInputMessageContext"]
                    ["toolResults"]
                    .as_array()
                    .cloned()
                    .unwrap_or_default();
                combined.extend(cur_results);
                prev["userInputMessage"]["userInputMessageContext"]["toolResults"] =
                    json!(combined);
            }
        } else {
            merged.push(current);
        }
    }
    merged
}

/// Guard 2: when tools ARE present, drop any `toolResults` entry (on any
/// history item or `currentMessage`) whose `toolUseId` has no matching
/// `toolUses` entry in any assistant history turn; fold its text back into
/// the carrying turn's content instead of leaving a dangling structured
/// reference (which makes Kiro 400). Delete the whole context object if it
/// becomes empty (no kept results, no tools).
fn reconcile_orphaned_tool_results(history: &mut [Value], current_message: Option<&mut Value>) {
    let mut valid_ids: std::collections::HashSet<String> = Default::default();
    for h in history.iter() {
        if let Some(tool_uses) = h["assistantResponseMessage"]["toolUses"].as_array() {
            for tu in tool_uses {
                if let Some(id) = tu["toolUseId"].as_str() {
                    valid_ids.insert(id.to_string());
                }
            }
        }
    }

    let mut carriers: Vec<&mut Value> = history.iter_mut().collect();
    if let Some(cm) = current_message {
        carriers.push(cm);
    }

    for item in carriers {
        let uim = match item.get_mut("userInputMessage") {
            Some(v) => v,
            None => continue,
        };
        let results = match uim["userInputMessageContext"]["toolResults"].as_array() {
            Some(arr) if !arr.is_empty() => arr.clone(),
            _ => continue,
        };

        let mut kept = Vec::new();
        let mut salvaged = Vec::new();
        for tr in results {
            let id = tr["toolUseId"].as_str().unwrap_or("");
            if valid_ids.contains(id) {
                kept.push(tr);
            } else {
                let text = tr["content"]
                    .as_array()
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|c| c["text"].as_str())
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                salvaged.push(format!("[Tool result: {text}]"));
            }
        }
        if salvaged.is_empty() {
            continue;
        }

        let extra = salvaged.join("\n");
        let existing = uim["content"].as_str().unwrap_or("").to_string();
        uim["content"] = json!(if existing.is_empty() {
            extra
        } else {
            format!("{existing}\n\n{extra}")
        });

        let has_tools = uim["userInputMessageContext"]["tools"]
            .as_array()
            .map(|a| !a.is_empty())
            .unwrap_or(false);
        if kept.is_empty() && !has_tools {
            if let Some(obj) = uim.as_object_mut() {
                obj.remove("userInputMessageContext");
            }
        } else {
            uim["userInputMessageContext"]["toolResults"] = json!(kept);
        }
    }
}

/// Attach tool specs to `currentMessage` only (history entries never carry
/// `tools` — see module doc). No-op if already present or `tools` is empty.
fn attach_tools_to_current(current_message: &mut Value, tools: Vec<Value>) {
    if tools.is_empty() {
        return;
    }
    if current_message["userInputMessage"]["userInputMessageContext"]["tools"]
        .as_array()
        .is_some()
    {
        return;
    }
    current_message["userInputMessage"]["userInputMessageContext"]["tools"] = json!(tools);
}

// ---------------------------------------------------------------------------
// Anthropic route
// ---------------------------------------------------------------------------

/// Guard 1 (claude route): when the client sent NO tools, collapse every
/// tool_use/tool_result block to plain text so no structured tool reference
/// survives to trip Kiro's "tools required" validator rule.
fn flatten_claude_tool_interactions(messages: &[Value]) -> Vec<Value> {
    messages
        .iter()
        .map(|msg| {
            let role = msg["role"].as_str().unwrap_or("user");
            match (role, msg["content"].as_array()) {
                ("assistant", Some(blocks)) => {
                    let parts: Vec<String> = blocks
                        .iter()
                        .filter_map(|b| match b["type"].as_str().unwrap_or("") {
                            "text" => b["text"].as_str().map(|s| s.to_string()),
                            "tool_use" => Some(tool_use_bracket_text(
                                b["name"].as_str().unwrap_or(""),
                                &b["input"],
                            )),
                            _ => None,
                        })
                        .collect();
                    let mut out = msg.clone();
                    out["content"] = json!(parts.join("\n"));
                    out
                }
                ("user", Some(blocks)) => {
                    let new_blocks: Vec<Value> = blocks
                        .iter()
                        .map(|b| {
                            if b["type"] == "tool_result" {
                                json!({"type": "text", "text": claude_tool_result_bracket_text(&b["content"])})
                            } else {
                                b.clone()
                            }
                        })
                        .collect();
                    let mut out = msg.clone();
                    out["content"] = json!(new_blocks);
                    out
                }
                _ => msg.clone(),
            }
        })
        .collect()
}

fn build_claude_history(messages: &[Value], model: &str) -> (Vec<Value>, Option<Value>) {
    let mut history: Vec<Value> = Vec::new();
    let mut pending_user: Vec<String> = Vec::new();
    let mut pending_assistant: Vec<String> = Vec::new();
    let mut pending_results: Vec<Value> = Vec::new();
    let mut current_role: Option<Role> = None;

    for msg in messages {
        let role = if msg["role"].as_str().unwrap_or("user") == "assistant" {
            Role::Assistant
        } else {
            Role::User
        };
        if let Some(prev) = current_role {
            if prev != role {
                flush_pending(
                    prev,
                    &mut history,
                    &mut pending_user,
                    &mut pending_assistant,
                    &mut pending_results,
                    model,
                );
            }
        }
        current_role = Some(role);

        match role {
            Role::User => match &msg["content"] {
                Value::String(s) => pending_user.push(s.clone()),
                Value::Array(blocks) => {
                    for b in blocks {
                        match b["type"].as_str().unwrap_or("") {
                            "text" => {
                                pending_user.push(b["text"].as_str().unwrap_or("").to_string())
                            }
                            "tool_result" => {
                                let text = claude_tool_result_struct_text(&b["content"]);
                                pending_results.push(json!({
                                    "toolUseId": b["tool_use_id"],
                                    "status": "success",
                                    "content": [{"text": text}]
                                }));
                            }
                            _ => {} // image / unknown: MVP skip (see module doc)
                        }
                    }
                }
                _ => {}
            },
            Role::Assistant => {
                let mut text_content = String::new();
                let mut tool_uses: Vec<Value> = Vec::new();
                match &msg["content"] {
                    Value::String(s) => text_content.push_str(s),
                    Value::Array(blocks) => {
                        for b in blocks {
                            match b["type"].as_str().unwrap_or("") {
                                "text" => text_content.push_str(b["text"].as_str().unwrap_or("")),
                                "tool_use" => {
                                    let input = if b["input"].is_null() {
                                        json!({})
                                    } else {
                                        b["input"].clone()
                                    };
                                    tool_uses.push(json!({
                                        "toolUseId": b["id"],
                                        "name": b["name"],
                                        "input": input
                                    }));
                                }
                                _ => {}
                            }
                        }
                    }
                    _ => {}
                }
                if !text_content.is_empty() {
                    pending_assistant.push(text_content);
                }
                if !tool_uses.is_empty() {
                    flush_pending(
                        Role::Assistant,
                        &mut history,
                        &mut pending_user,
                        &mut pending_assistant,
                        &mut pending_results,
                        model,
                    );
                    if let Some(last) = history.last_mut() {
                        if last.get("assistantResponseMessage").is_some() {
                            last["assistantResponseMessage"]["toolUses"] = json!(tool_uses);
                        }
                    }
                    current_role = None; // force a fresh flush cycle, matches 9router
                }
            }
        }
    }
    if let Some(r) = current_role {
        flush_pending(
            r,
            &mut history,
            &mut pending_user,
            &mut pending_assistant,
            &mut pending_results,
            model,
        );
    }

    let current_message = pop_current_message(&mut history);
    (history, current_message)
}

fn claude_system_text(sys: &Value) -> String {
    match sys {
        Value::String(s) => s.clone(),
        Value::Array(items) => items
            .iter()
            .map(|s| s["text"].as_str().unwrap_or("").to_string())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

pub fn anthropic_request_to_kiro(
    body: &Value,
    model: &str,
    data: &ConnectionData,
    conversation_id: &str,
) -> Value {
    let messages_in = body["messages"].as_array().cloned().unwrap_or_default();
    let tools = body["tools"].as_array().cloned().unwrap_or_default();
    let client_provided_tools = !tools.is_empty();

    let messages = if client_provided_tools {
        messages_in
    } else {
        flatten_claude_tool_interactions(&messages_in)
    };

    let (history, current_message) = build_claude_history(&messages, model);
    let mut history = merge_consecutive_user_turns(history);
    let mut current_message = current_message;

    if client_provided_tools {
        reconcile_orphaned_tool_results(&mut history, current_message.as_mut());
    }

    let mut current_message = current_message
        .unwrap_or_else(|| json!({"userInputMessage": {"content": "", "modelId": model}}));

    if client_provided_tools {
        attach_tools_to_current(&mut current_message, build_tool_specs_claude(&tools));
    }

    let mut final_content = current_message["userInputMessage"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();

    if let Some(sys) = body.get("system") {
        let sys_text = claude_system_text(sys);
        if !sys_text.is_empty() {
            final_content = format!("{sys_text}\n\n{final_content}");
        }
    }
    // Always-present current-time context prefix — goes ONLY on
    // currentMessage.content (never history), assembled as
    // `[Context: ...]\n\n<system>\n\n<user>` (matches 9router).
    let timestamp = chrono::Utc::now().to_rfc3339();
    final_content = format!("[Context: Current time is {timestamp}]\n\n{final_content}");

    current_message["userInputMessage"]["content"] = json!(final_content);
    current_message["userInputMessage"]["origin"] = json!("AI_EDITOR");

    let max_tokens = body["max_tokens"]
        .as_u64()
        .filter(|&n| n != 0)
        .unwrap_or(32000);

    let mut payload = json!({
        "conversationState": {
            "chatTriggerType": "MANUAL",
            "conversationId": conversation_id,
            "currentMessage": current_message,
            "history": history,
        }
    });
    attach_profile_arn(&mut payload, data);
    payload["inferenceConfig"] = json!({"maxTokens": max_tokens});
    payload
}

// ---------------------------------------------------------------------------
// OpenAI route
// ---------------------------------------------------------------------------

/// Guard 1 (openai route): mirrors [`flatten_claude_tool_interactions`] for
/// OpenAI-shaped messages (`role: tool`, `tool_calls`, and the
/// Anthropic-ish content-block shapes 9router's openai translator also
/// tolerates for interop).
fn flatten_openai_tool_interactions(messages: &[Value]) -> Vec<Value> {
    let mut out = Vec::new();
    for msg in messages {
        let role = msg["role"].as_str().unwrap_or("user");

        if role == "tool" {
            out.push(json!({"role": "user", "content": openai_tool_result_bracket_text(&msg["content"])}));
            continue;
        }

        if role == "assistant" {
            let mut parts: Vec<String> = Vec::new();
            match &msg["content"] {
                Value::Array(blocks) => {
                    for c in blocks {
                        if c["type"] == "tool_use" {
                            parts.push(tool_use_bracket_text(
                                c["name"].as_str().unwrap_or(""),
                                &c["input"],
                            ));
                        } else if c["type"] == "text" || c.get("text").is_some() {
                            if let Some(t) = c["text"].as_str() {
                                parts.push(t.to_string());
                            }
                        }
                    }
                }
                Value::String(s) => parts.push(s.clone()),
                _ => {}
            }
            for tc in msg["tool_calls"].as_array().cloned().unwrap_or_default() {
                parts.push(tool_use_bracket_text(
                    tc["function"]["name"].as_str().unwrap_or(""),
                    &tc["function"]["arguments"],
                ));
            }
            let joined = parts
                .into_iter()
                .filter(|s| !s.is_empty())
                .collect::<Vec<_>>()
                .join("\n");
            out.push(json!({"role": "assistant", "content": joined}));
            continue;
        }

        if role == "user" {
            if let Some(blocks) = msg["content"].as_array() {
                let new_blocks: Vec<Value> = blocks
                    .iter()
                    .map(|c| {
                        if c["type"] == "tool_result" {
                            json!({"type": "text", "text": openai_tool_result_bracket_text(&c["content"])})
                        } else {
                            c.clone()
                        }
                    })
                    .collect();
                let mut m = msg.clone();
                m["content"] = json!(new_blocks);
                out.push(m);
                continue;
            }
        }

        out.push(msg.clone());
    }
    out
}

fn build_openai_history(messages: &[Value], model: &str) -> (Vec<Value>, Option<Value>) {
    let mut history: Vec<Value> = Vec::new();
    let mut pending_user: Vec<String> = Vec::new();
    let mut pending_assistant: Vec<String> = Vec::new();
    let mut pending_results: Vec<Value> = Vec::new();
    let mut current_role: Option<Role> = None;

    for msg in messages {
        let orig_role = msg["role"].as_str().unwrap_or("user");
        // system/tool normalize to user for role-alternation purposes.
        let role = if orig_role == "assistant" {
            Role::Assistant
        } else {
            Role::User
        };

        if let Some(prev) = current_role {
            if prev != role {
                flush_pending(
                    prev,
                    &mut history,
                    &mut pending_user,
                    &mut pending_assistant,
                    &mut pending_results,
                    model,
                );
            }
        }
        current_role = Some(role);

        match role {
            Role::User => {
                let mut content = String::new();
                match &msg["content"] {
                    Value::String(s) => content = s.clone(),
                    Value::Array(blocks) => {
                        let text_parts: Vec<String> = blocks
                            .iter()
                            .filter(|c| c["type"] == "text" || c.get("text").is_some())
                            .map(|c| c["text"].as_str().unwrap_or("").to_string())
                            .collect();
                        content = text_parts.join("\n");

                        for block in blocks.iter().filter(|c| c["type"] == "tool_result") {
                            let text = openai_tool_result_struct_text(&block["content"]);
                            pending_results.push(json!({
                                "toolUseId": block["tool_use_id"],
                                "status": "success",
                                "content": [{"text": text}]
                            }));
                        }
                    }
                    _ => {}
                }

                if orig_role == "tool" {
                    let tool_content = msg["content"].as_str().unwrap_or("").to_string();
                    pending_results.push(json!({
                        "toolUseId": msg["tool_call_id"],
                        "status": "success",
                        "content": [{"text": tool_content}]
                    }));
                } else if !content.is_empty() {
                    pending_user.push(content);
                }
            }
            Role::Assistant => {
                let mut text_content = String::new();
                let mut tool_uses_raw: Vec<Value> = Vec::new();
                match &msg["content"] {
                    Value::Array(blocks) => {
                        let text_blocks: Vec<&str> = blocks
                            .iter()
                            .filter(|c| c["type"] == "text")
                            .filter_map(|c| c["text"].as_str())
                            .collect();
                        text_content = text_blocks.join("\n").trim().to_string();
                        tool_uses_raw = blocks
                            .iter()
                            .filter(|c| c["type"] == "tool_use")
                            .cloned()
                            .collect();
                    }
                    Value::String(s) => text_content = s.trim().to_string(),
                    _ => {}
                }
                if let Some(tc) = msg["tool_calls"].as_array() {
                    if !tc.is_empty() {
                        tool_uses_raw = tc.clone();
                    }
                }

                if !text_content.is_empty() {
                    pending_assistant.push(text_content);
                }

                if !tool_uses_raw.is_empty() {
                    flush_pending(
                        Role::Assistant,
                        &mut history,
                        &mut pending_user,
                        &mut pending_assistant,
                        &mut pending_results,
                        model,
                    );
                    let normalized: Vec<Value> = tool_uses_raw
                        .iter()
                        .map(|tc| {
                            let id = tc["id"]
                                .as_str()
                                .map(str::to_string)
                                .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
                            if tc.get("function").is_some() {
                                let name =
                                    tc["function"]["name"].as_str().unwrap_or("").to_string();
                                let input = safe_json_parse(
                                    tc["function"]["arguments"].as_str().unwrap_or(""),
                                );
                                json!({"toolUseId": id, "name": name, "input": input})
                            } else {
                                let input = if tc["input"].is_null() {
                                    json!({})
                                } else {
                                    tc["input"].clone()
                                };
                                json!({"toolUseId": id, "name": tc["name"], "input": input})
                            }
                        })
                        .collect();
                    if let Some(last) = history.last_mut() {
                        if last.get("assistantResponseMessage").is_some() {
                            last["assistantResponseMessage"]["toolUses"] = json!(normalized);
                        }
                    }
                    current_role = None;
                }
            }
        }
    }
    if let Some(r) = current_role {
        flush_pending(
            r,
            &mut history,
            &mut pending_user,
            &mut pending_assistant,
            &mut pending_results,
            model,
        );
    }

    let current_message = pop_current_message(&mut history);
    (history, current_message)
}

pub fn openai_request_to_kiro(
    body: &Value,
    model: &str,
    data: &ConnectionData,
    conversation_id: &str,
) -> Value {
    let messages_in = body["messages"].as_array().cloned().unwrap_or_default();
    let tools = body["tools"].as_array().cloned().unwrap_or_default();
    let client_provided_tools = !tools.is_empty();

    let messages = if client_provided_tools {
        messages_in
    } else {
        flatten_openai_tool_interactions(&messages_in)
    };

    let (history, current_message) = build_openai_history(&messages, model);
    let mut history = merge_consecutive_user_turns(history);
    let mut current_message = current_message;

    if client_provided_tools {
        reconcile_orphaned_tool_results(&mut history, current_message.as_mut());
    }

    let mut current_message = current_message
        .unwrap_or_else(|| json!({"userInputMessage": {"content": "", "modelId": model}}));

    if client_provided_tools {
        attach_tools_to_current(&mut current_message, build_tool_specs_openai(&tools));
    }

    // No top-level `system` field in OpenAI bodies: system messages are
    // folded in via the ordinary role normalization above (system -> user).
    // The current-time context prefix still goes on currentMessage.content
    // only (never history), assembled as `[Context: ...]\n\n<content>`.
    let content = current_message["userInputMessage"]["content"]
        .as_str()
        .unwrap_or("")
        .to_string();
    let timestamp = chrono::Utc::now().to_rfc3339();
    let final_content = format!("[Context: Current time is {timestamp}]\n\n{content}");
    current_message["userInputMessage"]["content"] = json!(final_content);
    current_message["userInputMessage"]["origin"] = json!("AI_EDITOR");

    let mut payload = json!({
        "conversationState": {
            "chatTriggerType": "MANUAL",
            "conversationId": conversation_id,
            "currentMessage": current_message,
            "history": history,
        }
    });
    attach_profile_arn(&mut payload, data);
    payload["inferenceConfig"] = json!({"maxTokens": 32000});
    payload
}

// ---------------------------------------------------------------------------
// Response stream: AWS event-stream frames -> OpenAI chat.completion.chunk
// ---------------------------------------------------------------------------

/// Kiro (CodeWhisperer) streaming response -> OpenAI `chat.completion.chunk`
/// values. Ported from 9router's `KiroExecutor.transformEventStreamToSSE`
/// (open-sse/executors/kiro.js) — one state machine per stream instance, fed
/// AWS event-stream frames (already parsed by [`crate::llm_router::aws_stream`])
/// one at a time via [`Self::feed`], terminated by [`Self::finish`] (client
/// EOF with no terminal frame seen) or a `messageStopEvent` frame (normal
/// completion) — whichever comes first; both paths go through the same
/// `finished`-guarded terminal chunk so at most one is ever emitted.
pub struct KiroToOpenAiStream {
    id: String,
    model: String,
    /// `toolUseId` -> OpenAI tool_call index, assigned in first-seen order.
    seen_tool_ids: std::collections::HashMap<String, usize>,
    next_tool_index: usize,
    /// Counter for the rare case a `toolUseEvent` omits `toolUseId`.
    anon_tool_counter: usize,
    /// Persists ACROSS frames: a `<thinking>` span can open in one frame and
    /// close in a later one.
    in_thinking: bool,
    has_reasoning_content: bool,
    has_tool_calls: bool,
    emitted_role: bool,
    finished: bool,
    input_tokens: i64,
    output_tokens: i64,
}

impl KiroToOpenAiStream {
    pub fn new(model: &str) -> Self {
        Self {
            id: format!("chatcmpl-{}", uuid::Uuid::new_v4()),
            model: model.to_string(),
            seen_tool_ids: Default::default(),
            next_tool_index: 0,
            anon_tool_counter: 0,
            in_thinking: false,
            has_reasoning_content: false,
            has_tool_calls: false,
            emitted_role: false,
            finished: false,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    fn chunk(&self, delta: Value, finish_reason: Option<&str>) -> Value {
        json!({
            "id": self.id,
            "object": "chat.completion.chunk",
            "model": self.model,
            "choices": [{
                "index": 0,
                "delta": delta,
                "finish_reason": finish_reason,
            }]
        })
    }

    /// Build a chunk from `delta`, tagging `role: "assistant"` onto it if
    /// this is the very first delta the stream has produced.
    fn emit(&mut self, delta: Value) -> Value {
        let delta = if self.emitted_role {
            delta
        } else {
            self.emitted_role = true;
            let mut obj = delta.as_object().cloned().unwrap_or_default();
            obj.insert("role".to_string(), json!("assistant"));
            Value::Object(obj)
        };
        self.chunk(delta, None)
    }

    /// Strip literal `<thinking>...</thinking>` spans from `content`,
    /// carrying the open/close state across frames — a span can open in one
    /// frame and close in a later one. Mirrors 9router's leaked-thinking-tag
    /// guard (Kiro's Claude models can leak these into the content stream;
    /// the real reasoning is routed separately via `reasoningContentEvent`).
    fn strip_thinking(&mut self, content: &str) -> String {
        if self.in_thinking {
            if let Some(pos) = content.find("</thinking>") {
                self.in_thinking = false;
                let after = &content[pos + "</thinking>".len()..];
                strip_one_leading_newline(after)
            } else {
                String::new() // drop entirely while inside the thinking block
            }
        } else if let Some(open) = content.find("<thinking>") {
            self.in_thinking = true;
            if let Some(close) = content.find("</thinking>") {
                self.in_thinking = false;
                let before = &content[..open];
                let after = &content[close + "</thinking>".len()..];
                format!("{before}{}", strip_one_leading_newline(after))
            } else {
                content[..open].to_string()
            }
        } else {
            content.to_string()
        }
    }

    /// Route one parsed AWS event-stream frame; returns zero or more OpenAI
    /// chunks (most frame types produce exactly one, several produce none).
    pub fn feed(&mut self, frame: &aws_stream::AwsFrame) -> Vec<Value> {
        let mut out = Vec::new();
        let payload = &frame.payload;
        match frame.event_type.as_deref().unwrap_or("") {
            "assistantResponseEvent" => {
                if let Some(raw) = payload["content"].as_str().filter(|s| !s.is_empty()) {
                    let content = self.strip_thinking(raw);
                    if content.is_empty() && self.has_reasoning_content {
                        // Stripped a whole thinking span to nothing and the
                        // reasoning it carried was already surfaced via
                        // reasoningContentEvent — skip the empty chunk.
                    } else {
                        out.push(self.emit(json!({"content": content})));
                    }
                }
            }
            "reasoningContentEvent" => {
                let reasoning = payload.get("reasoningContentEvent").unwrap_or(payload);
                let text = match reasoning {
                    Value::String(s) => s.clone(),
                    other => other["text"]
                        .as_str()
                        .or_else(|| other["content"].as_str())
                        .unwrap_or("")
                        .to_string(),
                };
                if !text.is_empty() {
                    self.has_reasoning_content = true;
                    out.push(self.emit(json!({"reasoning_content": text})));
                }
            }
            "codeEvent" => {
                if let Some(text) = payload["content"].as_str() {
                    out.push(self.emit(json!({"content": text})));
                }
            }
            "toolUseEvent" => {
                if !payload.is_null() {
                    self.has_tool_calls = true;
                    let items: Vec<&Value> = match payload.as_array() {
                        Some(arr) => arr.iter().collect(),
                        None => vec![payload],
                    };
                    for item in items {
                        let tool_call_id = match item["toolUseId"].as_str() {
                            Some(id) => id.to_string(),
                            None => {
                                let id = format!("call_{}", self.anon_tool_counter);
                                self.anon_tool_counter += 1;
                                id
                            }
                        };
                        let is_new = !self.seen_tool_ids.contains_key(&tool_call_id);
                        let index = if is_new {
                            let idx = self.next_tool_index;
                            self.next_tool_index += 1;
                            self.seen_tool_ids.insert(tool_call_id.clone(), idx);
                            idx
                        } else {
                            self.seen_tool_ids[&tool_call_id]
                        };
                        if is_new {
                            let name = item["name"].as_str().unwrap_or("");
                            out.push(self.emit(json!({"tool_calls": [{
                                "index": index, "id": tool_call_id, "type": "function",
                                "function": {"name": name, "arguments": ""}
                            }]})));
                        }
                        let args = match item.get("input") {
                            Some(Value::String(s)) => Some(s.clone()),
                            // Matches 9router's `typeof input === "object"`
                            // gate, which is also true for JSON arrays — a
                            // bare array `input` must be stringified into an
                            // arguments delta too, not silently dropped.
                            Some(v) if v.is_object() || v.is_array() => {
                                Some(serde_json::to_string(v).unwrap_or_else(|_| "{}".into()))
                            }
                            _ => None,
                        };
                        if let Some(args) = args {
                            out.push(self.chunk(
                                json!({"tool_calls": [{
                                    "index": index, "function": {"arguments": args}
                                }]}),
                                None,
                            ));
                        }
                    }
                }
            }
            "metricsEvent" => {
                let metrics = payload.get("metricsEvent").unwrap_or(payload);
                let input = metrics["inputTokens"].as_i64().unwrap_or(0);
                let output = metrics["outputTokens"].as_i64().unwrap_or(0);
                // Matches 9router: only overwrite once real numbers show up,
                // so a metricsEvent with neither field present is a no-op.
                if input > 0 || output > 0 {
                    self.input_tokens = input;
                    self.output_tokens = output;
                }
            }
            // A plain-text turn terminates with `messageStopEvent`; a TOOL-USE
            // turn instead terminates with `metadataEvent {stopReason:"TOOL_USE"}`
            // (observed on the live wire) — both are valid terminals, so emit
            // the finish chunk for either. Without the `metadataEvent` arm a
            // tool-use turn never sets the terminal and the stream is wrongly
            // reported as "ended without a terminal event".
            "messageStopEvent" | "metadataEvent" => out.extend(self.terminal_chunk()),
            // contextUsageEvent / meteringEvent: bookkeeping only upstream,
            // no client chunk here. Unknown event types: ignore.
            _ => {}
        }
        out
    }

    /// Terminal chunk carries the accumulated `metricsEvent` token counts as
    /// an OpenAI-shaped `usage` field (`prompt_tokens`/`completion_tokens`) —
    /// the same convention a real OpenAI-compatible upstream uses on its own
    /// final streamed chunk (see `stream_options.include_usage` in
    /// `translate::anthropic_to_openai_request`). Without this, the
    /// downstream `OpenAiToAnthropicStream`/`ResponsesStreamState` encoders
    /// (which read `chunk["usage"]`) would have no way to learn kiro's real
    /// token counts and the client-visible terminal event (Anthropic's
    /// `message_delta.usage`) would always read zero, even though kiro
    /// reported real numbers via `metricsEvent` before `messageStopEvent`.
    fn terminal_chunk(&mut self) -> Vec<Value> {
        if self.finished {
            return vec![];
        }
        self.finished = true;
        let finish = if self.has_tool_calls {
            "tool_calls"
        } else {
            "stop"
        };
        let mut v = self.chunk(json!({}), Some(finish));
        v["usage"] = json!({
            "prompt_tokens": self.input_tokens,
            "completion_tokens": self.output_tokens,
        });
        vec![v]
    }

    /// Emit the terminal chunk if the upstream connection closed (client
    /// EOF) without ever sending a `messageStopEvent` frame; a no-op if one
    /// was already emitted.
    pub fn finish(&mut self) -> Vec<Value> {
        self.terminal_chunk()
    }

    /// True once the terminal chunk has been emitted (via a `messageStopEvent`
    /// frame or a `finish()` call).
    pub fn saw_terminal(&self) -> bool {
        self.finished
    }

    /// Accumulated (input, output) token counts from the upstream
    /// `metricsEvent` frame(s) seen so far.
    pub fn usage(&self) -> (i64, i64) {
        (self.input_tokens, self.output_tokens)
    }

    /// Terminal error chunk in OpenAI shape — IDENTICAL to
    /// `translate::AnthropicToOpenAiStream::error_frame`. Emit this INSTEAD
    /// of `finish()` when the upstream stream errored mid-flight.
    pub fn error_frame(&self, message: &str) -> Value {
        json!({"error": {"message": message, "type": "api_error"}})
    }
}

fn strip_one_leading_newline(s: &str) -> String {
    s.strip_prefix('\n').unwrap_or(s).to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn anthropic_single_user_turn_with_tool() {
        let body = serde_json::json!({
            "system": "You are a helpful assistant.",
            "max_tokens": 8192,
            "messages": [{ "role": "user", "content": "What is the weather in Paris?" }],
            "tools": [{ "name": "get_weather", "description": "Get current weather",
                        "input_schema": { "type": "object", "properties": { "city": { "type": "string" } }, "required": ["city"] } }]
        });
        let data = ConnectionData::default(); // builder-id default
        let k = anthropic_request_to_kiro(&body, "claude-sonnet-4.5", &data, "conv-1");
        let cur = &k["conversationState"]["currentMessage"]["userInputMessage"];
        assert_eq!(cur["modelId"], "claude-sonnet-4.5");
        assert_eq!(cur["origin"], "AI_EDITOR");
        // Prefix pinned to the very start: `[Context:...]\n\n<system>\n\n<user>`.
        assert!(cur["content"]
            .as_str()
            .unwrap()
            .starts_with("[Context: Current time is "));
        assert!(cur["content"]
            .as_str()
            .unwrap()
            .contains("You are a helpful assistant."));
        assert!(cur["content"]
            .as_str()
            .unwrap()
            .contains("What is the weather in Paris?"));
        assert_eq!(
            cur["userInputMessageContext"]["tools"][0]["toolSpecification"]["name"],
            "get_weather"
        );
        assert_eq!(k["conversationState"]["chatTriggerType"], "MANUAL");
        assert_eq!(k["conversationState"]["conversationId"], "conv-1");
        assert_eq!(
            k["profileArn"],
            "arn:aws:codewhisperer:us-east-1:638616132270:profile/AAAACCCCXXXX"
        );
        assert_eq!(k["inferenceConfig"]["maxTokens"], 8192);
    }

    #[test]
    #[allow(clippy::field_reassign_with_default)] // matches the brief's given test verbatim
    fn account_bound_omits_default_arn() {
        let body = serde_json::json!({ "messages": [{ "role": "user", "content": "hi" }] });
        let mut data = ConnectionData::default();
        data.provider_specific = Some(serde_json::json!({ "authMethod": "idc" })); // account-bound, no profileArn
        let k = openai_request_to_kiro(&body, "claude-sonnet-5", &data, "c");
        assert!(k.get("profileArn").is_none()); // never the shared default for account-bound
    }

    #[test]
    fn consecutive_user_turns_merge_and_last_is_current() {
        let body = serde_json::json!({ "messages": [
            { "role": "user", "content": "a" },
            { "role": "user", "content": "b" },
        ]});
        let k = openai_request_to_kiro(&body, "m", &ConnectionData::default(), "c");
        assert_eq!(
            k["conversationState"]["history"].as_array().unwrap().len(),
            0
        );
        // The merged content is prefixed with the non-deterministic timestamp
        // line, so assert on structure rather than exact equality.
        let content = k["conversationState"]["currentMessage"]["userInputMessage"]["content"]
            .as_str()
            .unwrap();
        assert!(content.starts_with("[Context: Current time is "));
        assert!(content.ends_with("a\n\nb"));
    }

    #[test]
    fn anthropic_flatten_guard_flattens_tool_blocks_when_no_client_tools() {
        let body = serde_json::json!({
            "messages": [
                { "role": "user", "content": "call the tool" },
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "Paris"} }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "tu_1", "content": "sunny" }
                ]}
            ]
            // no "tools" field at all
        });
        let k = anthropic_request_to_kiro(&body, "m", &ConnectionData::default(), "c");
        let history = k["conversationState"]["history"].as_array().unwrap();
        assert_eq!(history[0]["userInputMessage"]["content"], "call the tool");
        assert!(history[1]["assistantResponseMessage"]["content"]
            .as_str()
            .unwrap()
            .contains("[Tool call: get_weather"));
        assert!(history[1]["assistantResponseMessage"]
            .get("toolUses")
            .is_none());
        let cur = &k["conversationState"]["currentMessage"]["userInputMessage"];
        assert!(cur["content"]
            .as_str()
            .unwrap()
            .contains("[Tool result: sunny]"));
        assert!(cur.get("userInputMessageContext").is_none());
    }

    #[test]
    fn anthropic_orphaned_tool_result_folds_into_text_when_tools_present() {
        let body = serde_json::json!({
            "tools": [{ "name": "get_weather", "description": "d", "input_schema": {} }],
            "messages": [
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "orphan_1", "content": "leftover" },
                    { "type": "text", "text": "what's next" }
                ]}
            ]
        });
        let k = anthropic_request_to_kiro(&body, "m", &ConnectionData::default(), "c");
        let cur = &k["conversationState"]["currentMessage"]["userInputMessage"];
        let content = cur["content"].as_str().unwrap();
        assert!(content.contains("what's next"));
        assert!(content.contains("[Tool result: leftover]"));
        assert!(cur["userInputMessageContext"]["toolResults"].is_null());
        assert_eq!(
            cur["userInputMessageContext"]["tools"][0]["toolSpecification"]["name"],
            "get_weather"
        );
    }

    #[test]
    fn anthropic_tool_use_and_tool_result_round_trip_through_history() {
        let body = serde_json::json!({
            "tools": [{ "name": "get_weather", "description": "d", "input_schema": {} }],
            "messages": [
                { "role": "user", "content": "call weather" },
                { "role": "assistant", "content": [
                    { "type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "Paris"} }
                ]},
                { "role": "user", "content": [
                    { "type": "tool_result", "tool_use_id": "tu_1", "content": "sunny and 20C" }
                ]}
            ]
        });
        let k = anthropic_request_to_kiro(&body, "m", &ConnectionData::default(), "c");
        let history = k["conversationState"]["history"].as_array().unwrap();
        assert_eq!(history.len(), 2);
        assert_eq!(history[0]["userInputMessage"]["content"], "call weather");
        assert_eq!(
            history[1]["assistantResponseMessage"]["toolUses"][0]["toolUseId"],
            "tu_1"
        );
        assert_eq!(
            history[1]["assistantResponseMessage"]["toolUses"][0]["name"],
            "get_weather"
        );
        assert_eq!(
            history[1]["assistantResponseMessage"]["toolUses"][0]["input"]["city"],
            "Paris"
        );

        let cur = &k["conversationState"]["currentMessage"]["userInputMessage"];
        assert_eq!(
            cur["userInputMessageContext"]["toolResults"][0]["toolUseId"],
            "tu_1"
        );
        assert_eq!(
            cur["userInputMessageContext"]["toolResults"][0]["content"][0]["text"],
            "sunny and 20C"
        );
        assert_eq!(
            cur["userInputMessageContext"]["tools"][0]["toolSpecification"]["name"],
            "get_weather"
        );
        assert!(cur["content"].as_str().unwrap().contains("continue"));
    }

    #[test]
    fn openai_tool_calls_round_trip_through_history() {
        let body = serde_json::json!({
            "tools": [{"type": "function", "function": {"name": "get_weather", "description": "d", "parameters": {"type": "object", "properties": {}}}}],
            "messages": [
                { "role": "user", "content": "weather please" },
                { "role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Tokyo\"}"}}
                ]},
                { "role": "tool", "tool_call_id": "call_1", "content": "rainy" }
            ]
        });
        let k = openai_request_to_kiro(&body, "m", &ConnectionData::default(), "c");
        let history = k["conversationState"]["history"].as_array().unwrap();
        assert_eq!(history[0]["userInputMessage"]["content"], "weather please");
        assert_eq!(
            history[1]["assistantResponseMessage"]["toolUses"][0]["toolUseId"],
            "call_1"
        );
        assert_eq!(
            history[1]["assistantResponseMessage"]["toolUses"][0]["name"],
            "get_weather"
        );
        assert_eq!(
            history[1]["assistantResponseMessage"]["toolUses"][0]["input"]["city"],
            "Tokyo"
        );

        let cur = &k["conversationState"]["currentMessage"]["userInputMessage"];
        assert_eq!(
            cur["userInputMessageContext"]["toolResults"][0]["toolUseId"],
            "call_1"
        );
        assert_eq!(
            cur["userInputMessageContext"]["toolResults"][0]["content"][0]["text"],
            "rainy"
        );
        assert_eq!(k["inferenceConfig"]["maxTokens"], 32000);
    }

    #[test]
    fn openai_flatten_guard_flattens_when_no_tools() {
        let body = serde_json::json!({
            "messages": [
                { "role": "user", "content": "weather please" },
                { "role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function", "function": {"name": "get_weather", "arguments": "{\"city\":\"Tokyo\"}"}}
                ]},
                { "role": "tool", "tool_call_id": "call_1", "content": "rainy" }
            ]
            // no "tools" field
        });
        let k = openai_request_to_kiro(&body, "m", &ConnectionData::default(), "c");
        let history = k["conversationState"]["history"].as_array().unwrap();
        assert!(history[1]["assistantResponseMessage"]["content"]
            .as_str()
            .unwrap()
            .contains("[Tool call: get_weather"));
        assert!(history[1]["assistantResponseMessage"]
            .get("toolUses")
            .is_none());
        let cur = &k["conversationState"]["currentMessage"]["userInputMessage"];
        assert!(cur["content"]
            .as_str()
            .unwrap()
            .contains("[Tool result: rainy]"));
        assert!(cur.get("userInputMessageContext").is_none());
    }

    #[test]
    fn context_prefix_is_on_current_message_only_and_appears_exactly_once() {
        // Multi-turn so history has real user/assistant content to inspect.
        let body = serde_json::json!({ "messages": [
            { "role": "user", "content": "first turn" },
            { "role": "assistant", "content": "first reply" },
            { "role": "user", "content": "second turn" },
        ]});
        let k = anthropic_request_to_kiro(&body, "m", &ConnectionData::default(), "c");

        let marker = "[Context: Current time is ";
        let cur_content = k["conversationState"]["currentMessage"]["userInputMessage"]["content"]
            .as_str()
            .unwrap();
        // Exactly once on currentMessage, pinned to the start.
        assert_eq!(cur_content.matches(marker).count(), 1);
        assert!(cur_content.starts_with(marker));
        assert!(cur_content.ends_with("second turn"));

        // Never on any history item (user or assistant content).
        let history = k["conversationState"]["history"].as_array().unwrap();
        assert_eq!(history.len(), 2);
        for item in history {
            let content = item["userInputMessage"]["content"]
                .as_str()
                .or_else(|| item["assistantResponseMessage"]["content"].as_str())
                .unwrap();
            assert!(!content.contains(marker));
        }
        assert_eq!(history[0]["userInputMessage"]["content"], "first turn");
        assert_eq!(
            history[1]["assistantResponseMessage"]["content"],
            "first reply"
        );
    }

    // -----------------------------------------------------------------
    // KiroToOpenAiStream (response side)
    // -----------------------------------------------------------------

    fn frame(event_type: &str, payload: Value) -> aws_stream::AwsFrame {
        aws_stream::AwsFrame {
            event_type: Some(event_type.to_string()),
            payload,
        }
    }

    #[test]
    fn text_then_stop() {
        let mut s = KiroToOpenAiStream::new("claude-sonnet-4.5");
        let mut out = s.feed(&frame(
            "assistantResponseEvent",
            json!({ "content": "Hello" }),
        ));
        out.extend(s.feed(&frame("messageStopEvent", Value::Null)));
        let joined: String = out
            .iter()
            .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
            .collect();
        assert_eq!(joined, "Hello");
        assert!(s.saw_terminal());
        assert_eq!(out.last().unwrap()["choices"][0]["finish_reason"], "stop");
    }

    #[test]
    fn tool_use_turn_terminates_on_metadata_event_not_message_stop() {
        // Live wire: a tool-use turn ends with `metadataEvent{stopReason:TOOL_USE}`
        // (then contextUsage/metering, then EOF) and NEVER a `messageStopEvent`.
        // The stream must recognize this as a valid terminal with a
        // `tool_calls` finish reason — otherwise the pump reports "ended
        // without a terminal event" and the whole turn fails.
        let mut s = KiroToOpenAiStream::new("claude-haiku-4.5");
        let mut out = s.feed(&frame(
            "toolUseEvent",
            json!({ "toolUseId": "t1", "name": "write", "input": "{\"path\":\"a.txt\"}" }),
        ));
        out.extend(s.feed(&frame("toolUseEvent", json!({ "toolUseId": "t1", "name": "write", "stop": true }))));
        assert!(!s.saw_terminal(), "not terminal until the metadata/stop event");
        out.extend(s.feed(&frame("metadataEvent", json!({ "stopReason": "TOOL_USE" }))));
        assert!(s.saw_terminal(), "metadataEvent must terminate the tool-use turn");
        assert_eq!(out.last().unwrap()["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn tool_call_fragments_accumulate() {
        let mut s = KiroToOpenAiStream::new("m");
        let mut out = s.feed(&frame(
            "toolUseEvent",
            json!({ "toolUseId": "t1", "name": "get_weather", "input": "" }),
        ));
        out.extend(s.feed(&frame(
            "toolUseEvent",
            json!({ "toolUseId": "t1", "input": "{\"city\":" }),
        )));
        out.extend(s.feed(&frame(
            "toolUseEvent",
            json!({ "toolUseId": "t1", "input": "\"Paris\"}" }),
        )));
        out.extend(s.feed(&frame("messageStopEvent", Value::Null)));
        let start = out
            .iter()
            .find(|c| c["choices"][0]["delta"]["tool_calls"][0]["id"] == "t1")
            .unwrap();
        assert_eq!(
            start["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
            "get_weather"
        );
        let args: String = out
            .iter()
            .filter_map(|c| {
                c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
            })
            .collect();
        assert_eq!(args, "{\"city\":\"Paris\"}");
        assert_eq!(
            out.last().unwrap()["choices"][0]["finish_reason"],
            "tool_calls"
        );
    }

    #[test]
    fn tool_use_array_input_is_stringified_not_dropped() {
        let mut s = KiroToOpenAiStream::new("m");
        let out = s.feed(&frame(
            "toolUseEvent",
            json!({ "toolUseId": "t1", "name": "batch", "input": ["a", "b", 1] }),
        ));
        let args: String = out
            .iter()
            .filter_map(|c| {
                c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
            })
            .collect();
        assert_eq!(args, "[\"a\",\"b\",1]");
    }

    #[test]
    fn strips_leaked_thinking_spans() {
        let mut s = KiroToOpenAiStream::new("m");
        let out = s.feed(&frame(
            "assistantResponseEvent",
            json!({ "content": "A<thinking>secret</thinking>B" }),
        ));
        let joined: String = out
            .iter()
            .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
            .collect();
        assert_eq!(joined, "AB");
    }

    #[test]
    fn thinking_span_split_across_frames_drops_enclosed_text() {
        let mut s = KiroToOpenAiStream::new("m");
        let mut out = s.feed(&frame(
            "assistantResponseEvent",
            json!({ "content": "before<thinking>" }),
        ));
        out.extend(s.feed(&frame(
            "assistantResponseEvent",
            json!({ "content": "secret</thinking>after" }),
        )));
        let joined: String = out
            .iter()
            .filter_map(|c| c["choices"][0]["delta"]["content"].as_str())
            .collect();
        assert_eq!(joined, "beforeafter");
    }

    #[test]
    fn metrics_event_populates_usage() {
        let mut s = KiroToOpenAiStream::new("m");
        assert_eq!(s.usage(), (0, 0));
        s.feed(&frame(
            "metricsEvent",
            json!({ "inputTokens": 42, "outputTokens": 7 }),
        ));
        assert_eq!(s.usage(), (42, 7));
    }

    #[test]
    fn finish_without_message_stop_emits_exactly_one_terminal() {
        let mut s = KiroToOpenAiStream::new("m");
        s.feed(&frame("assistantResponseEvent", json!({ "content": "hi" })));
        assert!(!s.saw_terminal());
        let first = s.finish();
        assert_eq!(first.len(), 1);
        assert_eq!(first[0]["choices"][0]["finish_reason"], "stop");
        assert!(s.saw_terminal());
        let second = s.finish();
        assert!(
            second.is_empty(),
            "finish() must not double-emit a terminal chunk"
        );
    }

    #[test]
    fn error_frame_matches_anthropic_to_openai_shape() {
        let s = KiroToOpenAiStream::new("m");
        let err = s.error_frame("upstream stream interrupted: boom");
        let expected = crate::llm_router::translate::AnthropicToOpenAiStream::new()
            .error_frame("upstream stream interrupted: boom");
        assert_eq!(err, expected);
    }

    #[test]
    fn first_delta_carries_role_regardless_of_frame_type() {
        let mut s = KiroToOpenAiStream::new("m");
        let out = s.feed(&frame(
            "toolUseEvent",
            json!({ "toolUseId": "t1", "name": "get_weather", "input": {} }),
        ));
        assert_eq!(out[0]["choices"][0]["delta"]["role"], "assistant");
        // subsequent deltas do not repeat role.
        let more = s.feed(&frame("assistantResponseEvent", json!({ "content": "hi" })));
        assert!(more[0]["choices"][0]["delta"].get("role").is_none());
    }
}
