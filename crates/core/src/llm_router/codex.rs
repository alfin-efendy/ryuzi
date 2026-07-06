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

use std::collections::HashMap;

/// Decodes a Codex Responses SSE stream into OpenAI `chat.completion.chunk`s.
/// Terminal on `response.completed` (or `finish()` for a clean EOF). Ported
/// from 9router open-sse/executors/codex.js streaming handler.
pub struct ResponsesToOpenAiStream {
    model: String,
    /// function_call item id -> tool_calls[] index, assigned first-seen.
    tool_index: HashMap<String, usize>,
    next_index: usize,
    has_tool_calls: bool,
    finished: bool,
    input_tokens: i64,
    output_tokens: i64,
}

impl ResponsesToOpenAiStream {
    pub fn new(model: &str) -> Self {
        Self { model: model.to_string(), tool_index: HashMap::new(), next_index: 0,
            has_tool_calls: false, finished: false, input_tokens: 0, output_tokens: 0 }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> Value {
        json!({"object": "chat.completion.chunk", "model": self.model,
            "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]})
    }

    pub fn feed(&mut self, event: &str, data: &Value) -> Vec<Value> {
        let mut out = Vec::new();
        match event {
            "response.output_text.delta" => {
                if let Some(t) = data.get("delta").and_then(Value::as_str) {
                    out.push(self.chunk(json!({"content": t}), None));
                }
            }
            "response.reasoning_summary_text.delta" | "response.reasoning_text.delta" => {
                if let Some(t) = data.get("delta").and_then(Value::as_str) {
                    out.push(self.chunk(json!({"reasoning_content": t}), None));
                }
            }
            "response.output_item.added" => {
                let item = data.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    self.has_tool_calls = true;
                    let id = item.get("call_id").and_then(Value::as_str)
                        .or_else(|| item.get("id").and_then(Value::as_str))
                        .unwrap_or("").to_string();
                    let index = *self.tool_index.entry(id.clone()).or_insert_with(|| {
                        let i = self.next_index; self.next_index += 1; i });
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                    out.push(self.chunk(json!({"tool_calls": [{"index": index, "id": id,
                        "type": "function", "function": {"name": name, "arguments": ""}}]}), None));
                }
            }
            "response.function_call_arguments.delta" => {
                let id = data.get("item_id").and_then(Value::as_str).unwrap_or("");
                if let Some(&index) = self.tool_index.get(id) {
                    if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                        out.push(self.chunk(json!({"tool_calls": [{"index": index,
                            "function": {"arguments": delta}}]}), None));
                    }
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(u) = data.get("response").and_then(|r| r.get("usage")) {
                    self.input_tokens = u.get("input_tokens").and_then(Value::as_i64).unwrap_or(0);
                    self.output_tokens = u.get("output_tokens").and_then(Value::as_i64).unwrap_or(0);
                }
                out.extend(self.terminal());
            }
            "response.failed" | "error" => {
                let msg = data.pointer("/response/error/message")
                    .or_else(|| data.pointer("/error/message"))
                    .and_then(Value::as_str).unwrap_or("codex upstream error");
                out.push(self.chunk(json!({"content": ""}), None));
                out.push(json!({"error": {"message": msg}}));
                self.finished = true;
            }
            _ => {}
        }
        out
    }

    fn terminal(&mut self) -> Vec<Value> {
        if self.finished { return vec![]; }
        self.finished = true;
        let finish = if self.has_tool_calls { "tool_calls" } else { "stop" };
        let mut c = self.chunk(json!({}), Some(finish));
        c["usage"] = json!({"prompt_tokens": self.input_tokens,
            "completion_tokens": self.output_tokens});
        vec![c]
    }

    /// Emit the terminal chunk if the stream closed (clean EOF) without a
    /// `response.completed`; a no-op if one was already emitted.
    pub fn finish(&mut self) -> Vec<Value> { self.terminal() }
    pub fn saw_terminal(&self) -> bool { self.finished }
    pub fn usage(&self) -> (i64, i64) { (self.input_tokens, self.output_tokens) }
}

// ---------------------------------------------------------------------------
// Codex Responses body normalization
// ---------------------------------------------------------------------------

const CODEX_DEFAULT_INSTRUCTIONS: &str =
    "You are Codex, based on GPT-5. You are running as a coding agent in the Codex CLI on a user's computer.";
const CODEX_ALLOWED_RESPONSE_FIELDS: &[&str] = &[
    "model",
    "input",
    "instructions",
    "tools",
    "tool_choice",
    "stream",
    "store",
    "reasoning",
    "service_tier",
    "include",
    "prompt_cache_key",
    "client_metadata",
    "text",
];

fn normalize_responses_input(input: Value) -> Value {
    match input {
        Value::String(s) => {
            let text = if s.is_empty() { "..." } else { s.as_str() };
            json!([{"type": "message", "role": "user",
                "content": [{"type": "input_text", "text": text}]}])
        }
        Value::Array(items) if !items.is_empty() => Value::Array(items),
        _ => json!([{"type": "message", "role": "user",
            "content": [{"type": "input_text", "text": "..."}]}]),
    }
}

fn convert_codex_system_items_to_developer(body: &mut Value) {
    let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    for item in items {
        let is_system = item
            .get("role")
            .and_then(Value::as_str)
            .map(|r| r == "system")
            .unwrap_or(false)
            && item
                .get("type")
                .and_then(Value::as_str)
                .map(|t| t == "message")
                .unwrap_or(true);
        if is_system {
            item["role"] = json!("developer");
        }
    }
}

fn strip_codex_stored_item_refs(body: &mut Value) {
    fn is_server_id(s: &str) -> bool {
        ["rs_", "fc_", "resp_", "msg_"]
            .iter()
            .any(|prefix| s.starts_with(prefix))
    }

    let Some(items) = body.get_mut("input").and_then(Value::as_array_mut) else {
        return;
    };
    items.retain_mut(|item| {
        if item.as_str().map(is_server_id).unwrap_or(false) {
            return false;
        }
        if item
            .get("type")
            .and_then(Value::as_str)
            .map(|t| t == "item_reference")
            .unwrap_or(false)
        {
            return false;
        }
        if item
            .get("id")
            .and_then(Value::as_str)
            .map(is_server_id)
            .unwrap_or(false)
        {
            if let Some(obj) = item.as_object_mut() {
                obj.remove("id");
            }
        }
        true
    });
}

fn normalize_codex_tools(body: &mut Value) {
    let Some(tools) = body.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    tools.retain_mut(|tool| {
        let Some(obj) = tool.as_object_mut() else {
            return false;
        };
        let tool_type = obj.get("type").and_then(Value::as_str).unwrap_or("");
        if tool_type != "function" {
            return matches!(
                tool_type,
                "custom"
                    | "image_generation"
                    | "web_search"
                    | "web_search_preview"
                    | "file_search"
                    | "computer"
                    | "computer_use_preview"
                    | "code_interpreter"
                    | "mcp"
                    | "local_shell"
                    | "tool_search"
            );
        }

        if obj.contains_key("function") {
            let Some(function) = obj.get("function").and_then(Value::as_object) else {
                return false;
            };
            let Some(name) = function.get("name").and_then(Value::as_str) else {
                return false;
            };
            let name = name.trim().chars().take(128).collect::<String>();
            if name.is_empty() {
                return false;
            }
            let description = function
                .get("description")
                .and_then(Value::as_str)
                .unwrap_or("")
                .to_string();
            let parameters = function
                .get("parameters")
                .cloned()
                .unwrap_or_else(|| json!({"type": "object", "properties": {}}));
            obj.clear();
            obj.insert("type".into(), json!("function"));
            obj.insert("name".into(), json!(name));
            if !description.is_empty() {
                obj.insert("description".into(), json!(description));
            }
            obj.insert("parameters".into(), parameters);
        }
        true
    });
}

pub fn codex_virtual_model_to_upstream(model: &str) -> (String, Option<&'static str>) {
    let mut upstream = model.strip_suffix("-review").unwrap_or(model).to_string();
    for effort in ["xhigh", "high", "medium", "low", "none"] {
        let suffix = format!("-{effort}");
        if upstream.ends_with(&suffix) {
            let new_len = upstream.len() - suffix.len();
            upstream.truncate(new_len);
            return (upstream, Some(effort));
        }
    }
    (upstream, None)
}

pub fn normalize_codex_responses_body(body: &mut Value, upstream_model: &str, cache_key: Option<&str>) {
    let (model, model_effort) = codex_virtual_model_to_upstream(upstream_model);
    body["model"] = json!(model);
    let input = body.get("input").cloned().unwrap_or(Value::Null);
    body["input"] = normalize_responses_input(input);
    convert_codex_system_items_to_developer(body);
    strip_codex_stored_item_refs(body);
    normalize_codex_tools(body);

    body["stream"] = json!(true);
    body["store"] = json!(false);
    if body
        .get("instructions")
        .and_then(Value::as_str)
        .map(str::trim)
        .unwrap_or("")
        .is_empty()
    {
        body["instructions"] = json!(CODEX_DEFAULT_INSTRUCTIONS);
    }
    if body.get("prompt_cache_key").is_none() {
        if let Some(key) = cache_key {
            body["prompt_cache_key"] = json!(key);
        }
    }

    if body.get("reasoning").is_none() {
        let effort = body
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .or(model_effort)
            .unwrap_or("low");
        body["reasoning"] = json!({"effort": effort, "summary": "auto"});
    } else if body
        .get("reasoning")
        .and_then(|r| r.get("summary"))
        .is_none()
    {
        body["reasoning"]["summary"] = json!("auto");
    }
    if body
        .get("reasoning")
        .and_then(|r| r.get("effort"))
        .and_then(Value::as_str)
        .map(|e| e != "none")
        .unwrap_or(false)
    {
        body["include"] = json!(["reasoning.encrypted_content"]);
    }

    for key in [
        "temperature",
        "top_p",
        "frequency_penalty",
        "presence_penalty",
        "logprobs",
        "top_logprobs",
        "n",
        "seed",
        "max_tokens",
        "max_completion_tokens",
        "max_output_tokens",
        "user",
        "prompt_cache_retention",
        "metadata",
        "stream_options",
        "safety_identifier",
        "previous_response_id",
        "reasoning_effort",
    ] {
        if let Some(obj) = body.as_object_mut() {
            obj.remove(key);
        }
    }
    if let Some(obj) = body.as_object_mut() {
        obj.retain(|k, _| CODEX_ALLOWED_RESPONSE_FIELDS.contains(&k.as_str()));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn stream() -> ResponsesToOpenAiStream {
        ResponsesToOpenAiStream::new("gpt-5.2-codex")
    }

    #[test]
    fn output_text_delta_becomes_content_chunk() {
        let mut s = stream();
        let out = s.feed("response.output_text.delta", &json!({"delta": "Hello"}));
        assert_eq!(out[0]["choices"][0]["delta"]["content"], "Hello");
        assert!(!s.saw_terminal());
    }

    #[test]
    fn streamed_function_call_accumulates_and_finishes_as_tool_calls() {
        let mut s = stream();
        let mut out = s.feed("response.output_item.added",
            &json!({"item": {"type": "function_call", "call_id": "call_1", "name": "write"}}));
        out.extend(s.feed("response.function_call_arguments.delta",
            &json!({"item_id": "call_1", "delta": "{\"path\":"})));
        out.extend(s.feed("response.function_call_arguments.delta",
            &json!({"item_id": "call_1", "delta": "\"a.txt\"}"})));
        out.extend(s.feed("response.completed",
            &json!({"response": {"usage": {"input_tokens": 12, "output_tokens": 7}}})));
        let start = out.iter().find(|c| c["choices"][0]["delta"]["tool_calls"][0]["id"] == "call_1").unwrap();
        assert_eq!(start["choices"][0]["delta"]["tool_calls"][0]["function"]["name"], "write");
        let args: String = out.iter()
            .filter_map(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str())
            .collect();
        assert_eq!(args, "{\"path\":\"a.txt\"}");
        assert_eq!(out.last().unwrap()["choices"][0]["finish_reason"], "tool_calls");
        assert!(s.saw_terminal());
        assert_eq!(s.usage(), (12, 7));
    }

    #[test]
    fn clean_eof_without_completed_still_finishes() {
        let mut s = stream();
        let _ = s.feed("response.output_text.delta", &json!({"delta": "hi"}));
        assert!(!s.saw_terminal());
        let out = s.finish();
        assert_eq!(out.last().unwrap()["choices"][0]["finish_reason"], "stop");
        assert!(s.saw_terminal());
    }

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
    fn codex_oauth_responses_body_is_normalized_for_backend() {
        let mut body = json!({
            "model": "openai-oauth/gpt-5.3-codex-high",
            "input": "hi",
            "temperature": 0.2,
            "max_output_tokens": 128,
            "previous_response_id": "resp_old",
            "stream": false
        });

        normalize_codex_responses_body(&mut body, "gpt-5.3-codex-high", Some("session-1"));

        assert_eq!(body["model"], "gpt-5.3-codex");
        assert_eq!(body["stream"], true);
        assert_eq!(body["store"], false);
        assert!(body["instructions"]
            .as_str()
            .unwrap_or("")
            .contains("Codex"));
        assert_eq!(body["prompt_cache_key"], "session-1");
        assert_eq!(body["reasoning"]["effort"], "high");
        assert_eq!(body["reasoning"]["summary"], "auto");
        assert_eq!(body["include"][0], "reasoning.encrypted_content");
        assert!(body.get("temperature").is_none());
        assert!(body.get("max_output_tokens").is_none());
        assert!(body.get("previous_response_id").is_none());
        assert!(body.get("reasoning_effort").is_none());
        assert!(body["input"].as_array().is_some());
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
