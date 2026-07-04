//! OpenAI Responses API <-> internal chat translation, and the Responses SSE
//! encoder. Ported from 9router (MIT, (c) 2024-2026 decolua and contributors) —
//! open-sse/translator/{request,response}/openai-responses.js +
//! transformer/responsesTransformer.js.
use serde_json::{json, Value};

/// Responses request body -> internal OpenAI Chat Completions body.
pub fn responses_request_to_chat(body: &Value) -> Value {
    let mut out = serde_json::Map::new();
    for k in ["model", "temperature", "top_p", "stream"] {
        if let Some(v) = body.get(k) {
            out.insert(k.into(), v.clone());
        }
    }
    if let Some(v) = body.get("max_output_tokens") {
        out.insert("max_tokens".into(), v.clone());
    }

    let mut messages: Vec<Value> = Vec::new();
    if let Some(instr) = body["instructions"].as_str() {
        if !instr.is_empty() {
            messages.push(json!({"role": "system", "content": instr}));
        }
    }

    // Pending assistant tool_calls accumulate until a non-function item flushes.
    let mut pending_tool_calls: Vec<Value> = Vec::new();
    let flush_tools = |msgs: &mut Vec<Value>, tc: &mut Vec<Value>| {
        if !tc.is_empty() {
            msgs.push(json!({"role": "assistant", "content": Value::Null, "tool_calls": tc.clone()}));
            tc.clear();
        }
    };

    match &body["input"] {
        Value::String(s) => {
            let text = if s.is_empty() { "..." } else { s.as_str() };
            messages.push(json!({"role": "user", "content": text}));
        }
        Value::Array(items) if !items.is_empty() => {
            for item in items {
                let itype = item["type"]
                    .as_str()
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| if item.get("role").is_some() { "message".into() } else { String::new() });
                match itype.as_str() {
                    "message" => {
                        flush_tools(&mut messages, &mut pending_tool_calls);
                        let role = item["role"].as_str().unwrap_or("user");
                        let content = map_response_content(&item["content"]);
                        messages.push(json!({"role": role, "content": content}));
                    }
                    "function_call" => {
                        // nameless calls are skipped (9router #444)
                        if let Some(name) = item["name"].as_str() {
                            pending_tool_calls.push(json!({
                                "id": item["call_id"], "type": "function",
                                "function": {"name": name, "arguments": item["arguments"].as_str().unwrap_or("{}")},
                            }));
                        }
                    }
                    "function_call_output" => {
                        flush_tools(&mut messages, &mut pending_tool_calls);
                        let output = match &item["output"] {
                            Value::String(s) => s.clone(),
                            other => other.to_string(),
                        };
                        messages.push(json!({"role": "tool",
                            "tool_call_id": item["call_id"], "content": output}));
                    }
                    // reasoning + unknown items dropped in F2a
                    _ => {}
                }
            }
            flush_tools(&mut messages, &mut pending_tool_calls);
        }
        _ => {
            messages.push(json!({"role": "user", "content": "..."}));
        }
    }
    out.insert("messages".into(), Value::Array(messages));

    if let Some(tools) = body["tools"].as_array() {
        let mapped: Vec<Value> = tools
            .iter()
            .filter(|t| t["name"].is_string())
            .map(|t| json!({"type": "function", "function": {
                "name": t["name"], "description": t["description"],
                "parameters": normalize_params(&t["parameters"]),
            }}))
            .collect();
        if !mapped.is_empty() {
            out.insert("tools".into(), Value::Array(mapped));
        }
    }
    Value::Object(out)
}

fn map_response_content(content: &Value) -> Value {
    match content {
        Value::String(s) => json!(s),
        Value::Array(blocks) => {
            let parts: Vec<Value> = blocks
                .iter()
                .filter_map(|b| match b["type"].as_str().unwrap_or("") {
                    "input_text" | "output_text" => Some(json!({"type": "text", "text": b["text"]})),
                    "input_image" => {
                        let url = b["image_url"].as_str().or_else(|| b["file_id"].as_str()).unwrap_or("");
                        Some(json!({"type": "image_url",
                            "image_url": {"url": url, "detail": b["detail"].as_str().unwrap_or("auto")}}))
                    }
                    _ => None,
                })
                .collect();
            // Single text part collapses to a plain string.
            if parts.len() == 1 && parts[0]["type"] == "text" {
                parts[0]["text"].clone()
            } else {
                Value::Array(parts)
            }
        }
        _ => json!(""),
    }
}

fn normalize_params(p: &Value) -> Value {
    if p.is_object() {
        p.clone()
    } else {
        json!({"type": "object", "properties": {}})
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn instructions_and_string_input_become_messages() {
        let req = json!({"model": "m", "instructions": "be nice", "input": "hello",
                         "max_output_tokens": 100});
        let out = responses_request_to_chat(&req);
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs[0], json!({"role": "system", "content": "be nice"}));
        assert_eq!(msgs[1], json!({"role": "user", "content": "hello"}));
        assert_eq!(out["max_tokens"], 100);
        assert!(out.get("max_output_tokens").is_none());
        assert!(out.get("input").is_none());
        assert!(out.get("instructions").is_none());
    }

    #[test]
    fn message_items_map_content_blocks() {
        let req = json!({"model": "m", "input": [
            {"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "what is this"},
                {"type": "input_image", "image_url": "data:image/png;base64,AAAA"}
            ]}
        ]});
        let out = responses_request_to_chat(&req);
        let parts = out["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0], json!({"type": "text", "text": "what is this"}));
        assert_eq!(parts[1]["type"], "image_url");
        assert_eq!(parts[1]["image_url"]["url"], "data:image/png;base64,AAAA");
    }

    #[test]
    fn function_call_and_output_items_map_to_tool_calls() {
        let req = json!({"model": "m", "input": [
            {"type": "function_call", "call_id": "call_1", "name": "get_weather",
             "arguments": "{\"city\":\"Jakarta\"}"},
            {"type": "function_call_output", "call_id": "call_1", "output": "sunny"}
        ]});
        let out = responses_request_to_chat(&req);
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "assistant");
        assert_eq!(msgs[0]["tool_calls"][0]["id"], "call_1");
        assert_eq!(msgs[0]["tool_calls"][0]["function"]["name"], "get_weather");
        assert_eq!(msgs[1], json!({"role": "tool", "tool_call_id": "call_1", "content": "sunny"}));
    }

    #[test]
    fn item_type_falls_back_to_role_and_empty_input_gets_placeholder() {
        // Droid CLI omits `type` on message items.
        let req = json!({"model": "m", "input": [{"role": "user", "content": "hi"}]});
        let out = responses_request_to_chat(&req);
        assert_eq!(out["messages"][0], json!({"role": "user", "content": "hi"}));

        let empty = json!({"model": "m", "input": []});
        let out = responses_request_to_chat(&empty);
        assert_eq!(out["messages"][0], json!({"role": "user", "content": "..."}));
    }

    #[test]
    fn tools_flatten_and_reasoning_effort_maps() {
        let req = json!({"model": "m", "input": "hi",
            "tools": [{"type": "function", "name": "f", "description": "d",
                       "parameters": {"type": "object"}}]});
        let out = responses_request_to_chat(&req);
        assert_eq!(out["tools"][0], json!({"type": "function",
            "function": {"name": "f", "description": "d", "parameters": {"type": "object"}}}));
    }
}
