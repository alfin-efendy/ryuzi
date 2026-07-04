//! OpenAI ↔ Anthropic request/response translation over serde_json::Value.
//! Ported from 9router (MIT, (c) 2024-2026 decolua and contributors) —
//! mapping semantics from open-sse/translator/{request,response}.
use serde_json::{json, Value};

pub fn oai_finish_to_anthropic(reason: &str) -> &'static str {
    match reason {
        "length" => "max_tokens",
        "tool_calls" => "tool_use",
        _ => "end_turn",
    }
}

pub fn anthropic_stop_to_oai(reason: &str) -> &'static str {
    match reason {
        "max_tokens" => "length",
        "tool_use" => "tool_calls",
        _ => "stop", // end_turn, stop_sequence, unknown
    }
}

fn as_text(content: &Value) -> String {
    match content {
        Value::String(s) => s.clone(),
        Value::Array(blocks) => blocks
            .iter()
            .filter(|b| b["type"] == "text")
            .filter_map(|b| b["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n"),
        _ => String::new(),
    }
}

/// tool_result/tool content may be a plain string or content blocks; keep a
/// plain string as-is, otherwise flatten blocks to a text string.
fn tool_content_text(v: &Value) -> Value {
    match v {
        Value::String(_) => v.clone(),
        other => json!(as_text(other)),
    }
}

/// Copy shared sampling params (both APIs use the same names).
fn copy_common(src: &Value, dst: &mut serde_json::Map<String, Value>) {
    for k in ["temperature", "top_p", "stream", "metadata"] {
        if let Some(v) = src.get(k) {
            dst.insert(k.into(), v.clone());
        }
    }
    if let Some(m) = src.get("model") {
        dst.insert("model".into(), m.clone());
    }
}

pub fn anthropic_to_openai_request(body: &Value) -> anyhow::Result<Value> {
    let mut out = serde_json::Map::new();
    copy_common(body, &mut out);
    if let Some(v) = body.get("max_tokens") {
        out.insert("max_tokens".into(), v.clone());
    }
    if let Some(v) = body.get("stop_sequences") {
        out.insert("stop".into(), v.clone());
    }

    let mut messages: Vec<Value> = Vec::new();
    // system: string or [{type:text}] blocks → one system message.
    if let Some(sys) = body.get("system") {
        let text = as_text(sys);
        if !text.is_empty() {
            messages.push(json!({"role": "system", "content": text}));
        }
    }
    for m in body["messages"].as_array().cloned().unwrap_or_default() {
        let role = m["role"].as_str().unwrap_or("user").to_string();
        match &m["content"] {
            Value::String(s) => messages.push(json!({"role": role, "content": s})),
            Value::Array(blocks) => {
                let mut text_parts: Vec<Value> = Vec::new();
                let mut tool_calls: Vec<Value> = Vec::new();
                let mut tool_results: Vec<Value> = Vec::new();
                for b in blocks {
                    match b["type"].as_str().unwrap_or("") {
                        "text" => text_parts.push(json!({"type": "text", "text": b["text"]})),
                        "image" => {
                            let mt = b["source"]["media_type"].as_str().unwrap_or("image/png");
                            let data = b["source"]["data"].as_str().unwrap_or("");
                            text_parts.push(json!({
                                "type": "image_url",
                                "image_url": {"url": format!("data:{mt};base64,{data}")}
                            }));
                        }
                        "tool_use" => tool_calls.push(json!({
                            "id": b["id"], "type": "function",
                            "function": {
                                "name": b["name"],
                                "arguments": serde_json::to_string(&b["input"])?,
                            }
                        })),
                        "tool_result" => tool_results.push(json!({
                            "role": "tool",
                            "tool_call_id": b["tool_use_id"],
                            "content": tool_content_text(&b["content"]),
                        })),
                        _ => {}
                    }
                }
                // Assistant text collapses to a plain string when there are no
                // image parts (OpenAI requires string content for assistants).
                if role == "assistant" {
                    let text = text_parts
                        .iter()
                        .filter_map(|p| p["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("");
                    let mut msg = serde_json::Map::new();
                    msg.insert("role".into(), json!("assistant"));
                    // Tool-only turns (no text) must send content: null, not "",
                    // to match OpenAI's own tool-call message shape.
                    if text.is_empty() && !tool_calls.is_empty() {
                        msg.insert("content".into(), Value::Null);
                    } else {
                        msg.insert("content".into(), json!(text));
                    }
                    if !tool_calls.is_empty() {
                        msg.insert("tool_calls".into(), Value::Array(tool_calls));
                    }
                    messages.push(Value::Object(msg));
                } else if !text_parts.is_empty() {
                    // Single text part → plain string; mixed parts stay array.
                    let only_text = text_parts.len() == 1 && text_parts[0]["type"] == "text";
                    let content = if only_text {
                        text_parts[0]["text"].clone()
                    } else {
                        Value::Array(text_parts)
                    };
                    messages.push(json!({"role": "user", "content": content}));
                }
                messages.extend(tool_results);
            }
            _ => {}
        }
    }
    out.insert("messages".into(), Value::Array(messages));

    if let Some(tools) = body["tools"].as_array() {
        let mapped: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t["name"], "description": t["description"],
                    "parameters": t["input_schema"],
                }})
            })
            .collect();
        out.insert("tools".into(), Value::Array(mapped));
    }
    if let Some(tc) = body.get("tool_choice") {
        let mapped = match tc["type"].as_str().unwrap_or("auto") {
            "any" => json!("required"),
            "tool" => json!({"type": "function", "function": {"name": tc["name"]}}),
            "none" => json!("none"),
            _ => json!("auto"),
        };
        out.insert("tool_choice".into(), mapped);
    }
    Ok(Value::Object(out))
}

pub fn openai_to_anthropic_request(body: &Value) -> anyhow::Result<Value> {
    let mut out = serde_json::Map::new();
    copy_common(body, &mut out);
    out.insert(
        "max_tokens".into(),
        body.get("max_tokens")
            .or_else(|| body.get("max_completion_tokens"))
            .cloned()
            .unwrap_or(json!(4096)),
    );
    if let Some(v) = body.get("stop") {
        let arr = if v.is_string() { json!([v]) } else { v.clone() };
        out.insert("stop_sequences".into(), arr);
    }

    let mut system_parts: Vec<String> = Vec::new();
    let mut messages: Vec<Value> = Vec::new();
    for m in body["messages"].as_array().cloned().unwrap_or_default() {
        match m["role"].as_str().unwrap_or("user") {
            "system" | "developer" => system_parts.push(as_text(&m["content"])),
            "tool" => {
                let block = json!({
                    "type": "tool_result",
                    "tool_use_id": m["tool_call_id"],
                    "content": tool_content_text(&m["content"]),
                });
                // Anthropic rejects consecutive user turns: merge consecutive
                // OpenAI tool messages into the same user message's blocks.
                let can_merge = messages.last().is_some_and(|last| {
                    last["role"] == "user"
                        && last["content"].as_array().is_some_and(|arr| {
                            !arr.is_empty() && arr.iter().all(|b| b["type"] == "tool_result")
                        })
                });
                if can_merge {
                    if let Some(arr) = messages
                        .last_mut()
                        .and_then(|msg| msg.get_mut("content"))
                        .and_then(|c| c.as_array_mut())
                    {
                        arr.push(block);
                    }
                } else {
                    messages.push(json!({"role": "user", "content": [block]}));
                }
            }
            "assistant" => {
                let mut blocks: Vec<Value> = Vec::new();
                let text = as_text(&m["content"]);
                if !text.is_empty() {
                    blocks.push(json!({"type": "text", "text": text}));
                }
                for tc in m["tool_calls"].as_array().cloned().unwrap_or_default() {
                    let args = tc["function"]["arguments"].as_str().unwrap_or("{}");
                    let input: Value = serde_json::from_str(args).unwrap_or(json!({}));
                    blocks.push(json!({
                        "type": "tool_use", "id": tc["id"],
                        "name": tc["function"]["name"], "input": input,
                    }));
                }
                if !blocks.is_empty() {
                    messages.push(json!({"role": "assistant", "content": blocks}));
                }
            }
            _ => {
                // user: string stays a string; parts map text/image_url.
                match &m["content"] {
                    Value::Array(parts) => {
                        let blocks: Vec<Value> = parts
                            .iter()
                            .filter_map(|p| match p["type"].as_str().unwrap_or("") {
                                "text" => Some(json!({"type": "text", "text": p["text"]})),
                                "image_url" => {
                                    let url = p["image_url"]["url"].as_str().unwrap_or("");
                                    let (mt, data) = parse_data_uri(url)?;
                                    Some(json!({"type": "image", "source": {
                                        "type": "base64", "media_type": mt, "data": data,
                                    }}))
                                }
                                _ => None,
                            })
                            .collect();
                        messages.push(json!({"role": "user", "content": blocks}));
                    }
                    other => messages.push(json!({"role": "user", "content": other})),
                }
            }
        }
    }
    if !system_parts.is_empty() {
        out.insert("system".into(), json!(system_parts.join("\n\n")));
    }
    out.insert("messages".into(), Value::Array(messages));

    if let Some(tools) = body["tools"].as_array() {
        let mapped: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name": t["function"]["name"],
                    "description": t["function"]["description"],
                    "input_schema": t["function"]["parameters"],
                })
            })
            .collect();
        out.insert("tools".into(), Value::Array(mapped));
    }
    if let Some(tc) = body.get("tool_choice") {
        let mapped = match tc {
            Value::String(s) if s == "required" => json!({"type": "any"}),
            Value::String(s) if s == "none" => json!({"type": "none"}),
            Value::Object(_) => json!({"type": "tool", "name": tc["function"]["name"]}),
            _ => json!({"type": "auto"}),
        };
        out.insert("tool_choice".into(), mapped);
    }
    Ok(Value::Object(out))
}

fn parse_data_uri(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("data:")?;
    let (mt, data) = rest.split_once(";base64,")?;
    Some((mt.to_string(), data.to_string()))
}

pub fn openai_to_anthropic_response(resp: &Value) -> Value {
    let msg = &resp["choices"][0]["message"];
    let mut content: Vec<Value> = Vec::new();
    if let Some(text) = msg["content"].as_str() {
        if !text.is_empty() {
            content.push(json!({"type": "text", "text": text}));
        }
    }
    for tc in msg["tool_calls"].as_array().cloned().unwrap_or_default() {
        let args = tc["function"]["arguments"].as_str().unwrap_or("{}");
        let input: Value = serde_json::from_str(args).unwrap_or(json!({}));
        content.push(json!({
            "type": "tool_use", "id": tc["id"],
            "name": tc["function"]["name"], "input": input,
        }));
    }
    let finish = resp["choices"][0]["finish_reason"].as_str().unwrap_or("stop");
    json!({
        "id": resp["id"], "type": "message", "role": "assistant",
        "model": resp["model"], "content": content,
        "stop_reason": oai_finish_to_anthropic(finish), "stop_sequence": null,
        "usage": {
            "input_tokens": resp["usage"]["prompt_tokens"].as_i64().unwrap_or(0),
            "output_tokens": resp["usage"]["completion_tokens"].as_i64().unwrap_or(0),
        }
    })
}

pub fn anthropic_to_openai_response(resp: &Value) -> Value {
    let mut text = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    for b in resp["content"].as_array().cloned().unwrap_or_default().iter() {
        match b["type"].as_str().unwrap_or("") {
            "text" => text.push_str(b["text"].as_str().unwrap_or("")),
            "tool_use" => {
                let index = tool_calls.len();
                tool_calls.push(json!({
                    "index": index, "id": b["id"], "type": "function",
                    "function": {
                        "name": b["name"],
                        "arguments": serde_json::to_string(&b["input"]).unwrap_or_else(|_| "{}".into()),
                    }
                }));
            }
            _ => {}
        }
    }
    let mut message = serde_json::Map::new();
    message.insert("role".into(), json!("assistant"));
    message.insert("content".into(), json!(text));
    if !tool_calls.is_empty() {
        message.insert("tool_calls".into(), Value::Array(tool_calls));
    }
    let stop = resp["stop_reason"].as_str().unwrap_or("end_turn");
    let inp = resp["usage"]["input_tokens"].as_i64().unwrap_or(0);
    let outp = resp["usage"]["output_tokens"].as_i64().unwrap_or(0);
    json!({
        "id": resp["id"], "object": "chat.completion", "model": resp["model"],
        "created": crate::paths::now_ms() / 1000,
        "choices": [{"index": 0, "message": Value::Object(message),
                     "finish_reason": anthropic_stop_to_oai(stop), "logprobs": null}],
        "usage": {"prompt_tokens": inp, "completion_tokens": outp, "total_tokens": inp + outp}
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn anthropic_request_maps_to_openai() {
        let req = json!({
            "model": "m", "max_tokens": 1024, "temperature": 0.5,
            "system": "be nice",
            "stop_sequences": ["END"],
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": [
                    {"type": "text", "text": "let me check"},
                    {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "Jakarta"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tu_1", "content": "sunny"}
                ]}
            ],
            "tools": [{"name": "get_weather", "description": "d", "input_schema": {"type": "object"}}],
            "tool_choice": {"type": "auto"}
        });
        let out = anthropic_to_openai_request(&req).unwrap();
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs[0], json!({"role": "system", "content": "be nice"}));
        assert_eq!(msgs[1], json!({"role": "user", "content": "hi"}));
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[2]["content"], "let me check");
        assert_eq!(msgs[2]["tool_calls"][0]["id"], "tu_1");
        assert_eq!(msgs[2]["tool_calls"][0]["function"]["name"], "get_weather");
        assert_eq!(
            msgs[2]["tool_calls"][0]["function"]["arguments"].as_str().unwrap(),
            r#"{"city":"Jakarta"}"#
        );
        assert_eq!(msgs[3], json!({"role": "tool", "tool_call_id": "tu_1", "content": "sunny"}));
        assert_eq!(out["max_tokens"], 1024);
        assert_eq!(out["stop"], json!(["END"]));
        assert_eq!(out["tools"][0]["function"]["parameters"], json!({"type": "object"}));
        assert_eq!(out["tool_choice"], "auto");
        assert!(out.get("system").is_none());
        assert!(out.get("stop_sequences").is_none());
    }

    #[test]
    fn openai_request_maps_to_anthropic() {
        let req = json!({
            "model": "m",
            "messages": [
                {"role": "system", "content": "be nice"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": null, "tool_calls": [
                    {"id": "call_1", "type": "function",
                     "function": {"name": "get_weather", "arguments": "{\"city\":\"Jakarta\"}"}}
                ]},
                {"role": "tool", "tool_call_id": "call_1", "content": "sunny"}
            ],
            "tools": [{"type": "function", "function": {"name": "get_weather", "description": "d", "parameters": {"type": "object"}}}],
            "tool_choice": "required",
            "stop": ["END"]
        });
        let out = openai_to_anthropic_request(&req).unwrap();
        assert_eq!(out["system"], "be nice");
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs[0], json!({"role": "user", "content": "hi"}));
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["content"][0]["type"], "tool_use");
        assert_eq!(msgs[1]["content"][0]["input"], json!({"city": "Jakarta"}));
        assert_eq!(msgs[2]["role"], "user");
        assert_eq!(msgs[2]["content"][0]["type"], "tool_result");
        assert_eq!(msgs[2]["content"][0]["tool_use_id"], "call_1");
        // Anthropic requires max_tokens: default injected when absent.
        assert_eq!(out["max_tokens"], 4096);
        assert_eq!(out["stop_sequences"], json!(["END"]));
        assert_eq!(out["tools"][0]["input_schema"], json!({"type": "object"}));
        assert_eq!(out["tool_choice"], json!({"type": "any"}));
    }

    #[test]
    fn openai_response_maps_to_anthropic() {
        let resp = json!({
            "id": "chatcmpl-1", "model": "m",
            "choices": [{"index": 0, "finish_reason": "tool_calls", "message": {
                "role": "assistant", "content": "checking",
                "tool_calls": [{"id": "call_1", "type": "function",
                    "function": {"name": "get_weather", "arguments": "{\"city\":\"Jakarta\"}"}}]
            }}],
            "usage": {"prompt_tokens": 10, "completion_tokens": 5}
        });
        let out = openai_to_anthropic_response(&resp);
        assert_eq!(out["type"], "message");
        assert_eq!(out["role"], "assistant");
        assert_eq!(out["content"][0], json!({"type": "text", "text": "checking"}));
        assert_eq!(out["content"][1]["type"], "tool_use");
        assert_eq!(out["content"][1]["input"], json!({"city": "Jakarta"}));
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(out["usage"], json!({"input_tokens": 10, "output_tokens": 5}));
    }

    #[test]
    fn anthropic_response_maps_to_openai() {
        let resp = json!({
            "id": "msg_1", "model": "m", "type": "message", "role": "assistant",
            "content": [
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "Jakarta"}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 10, "output_tokens": 5}
        });
        let out = anthropic_to_openai_response(&resp);
        assert_eq!(out["object"], "chat.completion");
        let msg = &out["choices"][0]["message"];
        assert_eq!(msg["content"], "checking");
        assert_eq!(msg["tool_calls"][0]["function"]["name"], "get_weather");
        assert_eq!(out["choices"][0]["finish_reason"], "tool_calls");
        assert_eq!(out["usage"], json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15}));
    }

    #[test]
    fn stop_reason_mappings_roundtrip() {
        assert_eq!(oai_finish_to_anthropic("stop"), "end_turn");
        assert_eq!(oai_finish_to_anthropic("length"), "max_tokens");
        assert_eq!(oai_finish_to_anthropic("tool_calls"), "tool_use");
        assert_eq!(anthropic_stop_to_oai("end_turn"), "stop");
        assert_eq!(anthropic_stop_to_oai("stop_sequence"), "stop");
        assert_eq!(anthropic_stop_to_oai("max_tokens"), "length");
        assert_eq!(anthropic_stop_to_oai("tool_use"), "tool_calls");
    }

    #[test]
    fn image_blocks_map_to_data_uris() {
        let req = json!({
            "model": "m", "max_tokens": 10,
            "messages": [{"role": "user", "content": [
                {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "AAAA"}},
                {"type": "text", "text": "what is this"}
            ]}]
        });
        let out = anthropic_to_openai_request(&req).unwrap();
        let parts = out["messages"][0]["content"].as_array().unwrap();
        assert_eq!(parts[0]["type"], "image_url");
        assert_eq!(parts[0]["image_url"]["url"], "data:image/png;base64,AAAA");
        assert_eq!(parts[1], json!({"type": "text", "text": "what is this"}));
    }

    #[test]
    fn tool_choice_none_maps_to_openai_none() {
        let req = json!({
            "model": "m", "max_tokens": 10,
            "messages": [{"role": "user", "content": "hi"}],
            "tool_choice": {"type": "none"}
        });
        let out = anthropic_to_openai_request(&req).unwrap();
        assert_eq!(out["tool_choice"], json!("none"));
    }

    #[test]
    fn anthropic_response_tool_call_indices_are_sequential() {
        let resp = json!({
            "id": "msg_1", "model": "m", "type": "message", "role": "assistant",
            "content": [
                {"type": "text", "text": "checking"},
                {"type": "tool_use", "id": "tu_1", "name": "a", "input": {}},
                {"type": "tool_use", "id": "tu_2", "name": "b", "input": {}}
            ],
            "stop_reason": "tool_use",
            "usage": {"input_tokens": 1, "output_tokens": 1}
        });
        let out = anthropic_to_openai_response(&resp);
        let calls = out["choices"][0]["message"]["tool_calls"].as_array().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[0]["index"], 0);
        assert_eq!(calls[1]["index"], 1);
    }

    #[test]
    fn assistant_tool_only_content_is_null() {
        let req = json!({
            "model": "m", "max_tokens": 10,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "Jakarta"}}
                ]}
            ]
        });
        let out = anthropic_to_openai_request(&req).unwrap();
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["content"], Value::Null);
        assert_eq!(msgs[0]["tool_calls"][0]["function"]["name"], "get_weather");
    }

    #[test]
    fn consecutive_tool_messages_merge_into_one_user_turn() {
        let req = json!({
            "model": "m",
            "messages": [
                {"role": "user", "content": "hi"},
                {"role": "tool", "tool_call_id": "call_1", "content": "sunny"},
                {"role": "tool", "tool_call_id": "call_2", "content": "windy"}
            ]
        });
        let out = openai_to_anthropic_request(&req).unwrap();
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 2);
        assert_eq!(msgs[1]["role"], "user");
        let blocks = msgs[1]["content"].as_array().unwrap();
        assert_eq!(blocks.len(), 2);
        assert_eq!(blocks[0]["tool_use_id"], "call_1");
        assert_eq!(blocks[1]["tool_use_id"], "call_2");
    }
}
