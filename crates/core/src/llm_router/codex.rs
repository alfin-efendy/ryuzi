//! Codex (OpenAI Responses API) translation for the native runtime.
//!
//! The native runner speaks Anthropic Messages; Codex speaks the OpenAI
//! Responses API. This module owns the two directions ryuzi was missing —
//! an OpenAI-chat -> Responses REQUEST builder and a Responses-SSE -> OpenAI-
//! chat-chunk decoder — plus the shared Codex request normalizer (moved here
//! from `server.rs`). It performs NO client-impersonation: it is pure wire-
//! format translation. Ported from 9router (MIT) open-sse/translator/request/
//! openai-responses.js and open-sse/executors/codex.js.

use serde_json::{json, Value};

/// Codex caps `call_id` at 64 chars; clamp consistently so a tool_use id and
/// its later tool_result id still match after translation.
pub(crate) fn clamp_call_id(id: &str) -> String {
    id.chars().take(64).collect()
}

/// OpenAI **chat** body -> OpenAI **Responses** request body. The first
/// `system` message becomes top-level `instructions`; user/assistant text
/// becomes `message` input items; `assistant.tool_calls` become `function_call`
/// items; `role:tool` messages become `function_call_output` items; chat
/// `tools` become flat Responses function tools. `stream`/`store` are forced by
/// the downstream normalizer, but set here too for a self-consistent object.
pub fn openai_chat_to_responses_request(chat: &Value) -> Value {
    let mut out = json!({ "input": [], "stream": true, "store": false });
    if let Some(model) = chat.get("model") {
        out["model"] = model.clone();
    }
    if let Some(tc) = chat.get("tool_choice") {
        out["tool_choice"] = tc.clone();
    }

    let mut instructions_set = false;
    for msg in chat.get("messages").and_then(Value::as_array).into_iter().flatten() {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "system" if !instructions_set => {
                out["instructions"] = json!(message_text(msg));
                instructions_set = true;
            }
            "system" => {
                // A second system message becomes a developer message item.
                out["input"].as_array_mut().expect("input is an array").push(json!({"type": "message", "role": "developer",
                    "content": [{"type": "input_text", "text": message_text(msg)}]}));
            }
            "tool" => {
                let call_id = clamp_call_id(msg.get("tool_call_id").and_then(Value::as_str).unwrap_or(""));
                out["input"].as_array_mut().expect("input is an array").push(json!({"type": "function_call_output",
                    "call_id": call_id, "output": message_text(msg)}));
            }
            "assistant" => {
                let text = message_text(msg);
                if !text.is_empty() {
                    out["input"].as_array_mut().expect("input is an array").push(json!({"type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": text}]}));
                }
                for tc in msg.get("tool_calls").and_then(Value::as_array).into_iter().flatten() {
                    let call_id = clamp_call_id(tc.get("id").and_then(Value::as_str).unwrap_or(""));
                    let f = tc.get("function").cloned().unwrap_or(Value::Null);
                    out["input"].as_array_mut().expect("input is an array").push(json!({"type": "function_call", "call_id": call_id,
                        "name": f.get("name").cloned().unwrap_or(json!("")),
                        "arguments": f.get("arguments").and_then(Value::as_str).unwrap_or("").to_string()}));
                }
            }
            _ => {
                // user (and any other) -> input_text/input_image message item.
                out["input"].as_array_mut().expect("input is an array").push(json!({"type": "message", "role": "user",
                    "content": message_content_parts(msg)}));
            }
        }
    }

    if let Some(tools) = chat.get("tools").and_then(Value::as_array) {
        let flat: Vec<Value> = tools.iter().filter_map(flatten_tool).collect();
        if !flat.is_empty() {
            out["tools"] = json!(flat);
        }
    }
    out
}

/// Join a chat message's textual content (string, or array of {type:text|...}).
fn message_text(msg: &Value) -> String {
    match msg.get("content") {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Array(parts)) => parts
            .iter()
            .filter_map(|p| p.get("text").and_then(Value::as_str))
            .collect::<Vec<_>>()
            .join(""),
        _ => String::new(),
    }
}

/// user message content -> Responses content parts (input_text / input_image).
fn message_content_parts(msg: &Value) -> Value {
    match msg.get("content") {
        Some(Value::Array(parts)) => {
            let mapped: Vec<Value> = parts
                .iter()
                .map(|p| match p.get("type").and_then(Value::as_str) {
                    Some("image_url") => {
                        let url = p["image_url"]["url"].as_str().unwrap_or("");
                        json!({"type": "input_image", "image_url": url})
                    }
                    _ => json!({"type": "input_text",
                        "text": p.get("text").and_then(Value::as_str).unwrap_or("")}),
                })
                .collect();
            json!(mapped)
        }
        _ => json!([{"type": "input_text", "text": message_text(msg)}]),
    }
}

/// chat tool {type:function, function:{name,description,parameters}} ->
/// flat Responses {type:function, name, description?, parameters}.
fn flatten_tool(tool: &Value) -> Option<Value> {
    let f = tool.get("function")?;
    let name = f.get("name").and_then(Value::as_str)?;
    let mut out = json!({"type": "function", "name": name,
        "parameters": f.get("parameters").cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}))});
    if let Some(desc) = f.get("description").and_then(Value::as_str).filter(|s| !s.is_empty()) {
        out["description"] = json!(desc);
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn system_becomes_instructions_user_becomes_message_item() {
        let chat = json!({"model": "gpt-5.2-codex", "messages": [
            {"role": "system", "content": "be terse"},
            {"role": "user", "content": "hi"}
        ]});
        let out = openai_chat_to_responses_request(&chat);
        assert_eq!(out["instructions"], "be terse");
        assert_eq!(out["input"][0]["type"], "message");
        assert_eq!(out["input"][0]["role"], "user");
        assert_eq!(out["input"][0]["content"][0]["text"], "hi");
        assert_eq!(out["stream"], true);
        assert_eq!(out["store"], false);
    }

    #[test]
    fn tool_calls_and_tool_results_become_function_items_linked_by_call_id() {
        let chat = json!({"model": "m", "messages": [
            {"role": "user", "content": "make a file"},
            {"role": "assistant", "content": null,
             "tool_calls": [{"id": "call_abc", "type": "function",
                "function": {"name": "write", "arguments": "{\"path\":\"a.txt\"}"}}]},
            {"role": "tool", "tool_call_id": "call_abc", "content": "wrote 5 bytes"}
        ]});
        let out = openai_chat_to_responses_request(&chat);
        let items = out["input"].as_array().unwrap();
        let fc = items.iter().find(|i| i["type"] == "function_call").unwrap();
        assert_eq!(fc["call_id"], "call_abc");
        assert_eq!(fc["name"], "write");
        assert_eq!(fc["arguments"], "{\"path\":\"a.txt\"}");
        let fco = items.iter().find(|i| i["type"] == "function_call_output").unwrap();
        assert_eq!(fco["call_id"], "call_abc");
        assert_eq!(fco["output"], "wrote 5 bytes");
    }

    #[test]
    fn tools_are_flattened_and_call_id_is_clamped_to_64() {
        let chat = json!({"model": "m", "messages": [],
            "tools": [{"type": "function", "function":
                {"name": "read", "description": "read a file",
                 "parameters": {"type": "object", "properties": {"path": {"type": "string"}}}}}]});
        let out = openai_chat_to_responses_request(&chat);
        assert_eq!(out["tools"][0]["type"], "function");
        assert_eq!(out["tools"][0]["name"], "read");
        assert_eq!(out["tools"][0]["description"], "read a file");
        assert!(out["tools"][0]["parameters"]["properties"]["path"].is_object());
        assert_eq!(clamp_call_id(&"x".repeat(100)).len(), 64);
    }
}
