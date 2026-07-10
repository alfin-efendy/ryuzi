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
        out["tool_choice"] = flatten_tool_choice(tc);
    }

    let mut instructions_set = false;
    for msg in chat
        .get("messages")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let role = msg.get("role").and_then(Value::as_str).unwrap_or("");
        match role {
            "system" if !instructions_set => {
                out["instructions"] = json!(message_text(msg));
                instructions_set = true;
            }
            "system" => {
                // A second system message becomes a developer message item.
                out["input"]
                    .as_array_mut()
                    .expect("input is an array")
                    .push(json!({"type": "message", "role": "developer",
                    "content": [{"type": "input_text", "text": message_text(msg)}]}));
            }
            "tool" => {
                let call_id = clamp_call_id(
                    msg.get("tool_call_id")
                        .and_then(Value::as_str)
                        .unwrap_or(""),
                );
                out["input"]
                    .as_array_mut()
                    .expect("input is an array")
                    .push(json!({"type": "function_call_output",
                    "call_id": call_id, "output": message_text(msg)}));
            }
            "assistant" => {
                let text = message_text(msg);
                if !text.is_empty() {
                    out["input"]
                        .as_array_mut()
                        .expect("input is an array")
                        .push(json!({"type": "message", "role": "assistant",
                        "content": [{"type": "output_text", "text": text}]}));
                }
                for tc in msg
                    .get("tool_calls")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                {
                    let call_id = clamp_call_id(tc.get("id").and_then(Value::as_str).unwrap_or(""));
                    let f = tc.get("function").cloned().unwrap_or(Value::Null);
                    out["input"].as_array_mut().expect("input is an array").push(json!({"type": "function_call", "call_id": call_id,
                        "name": f.get("name").cloned().unwrap_or(json!("")),
                        "arguments": f.get("arguments").and_then(Value::as_str).unwrap_or("").to_string()}));
                }
            }
            _ => {
                // user (and any other) -> input_text/input_image message item.
                out["input"]
                    .as_array_mut()
                    .expect("input is an array")
                    .push(json!({"type": "message", "role": "user",
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

/// OpenAI-chat `tool_choice` -> Responses `tool_choice`. String forms
/// ("auto"/"required"/"none") pass through unchanged. A forced-tool OBJECT is
/// the nested chat shape `{"type":"function","function":{"name":N}}` (as
/// produced by `translate::anthropic_to_openai_request`); the Responses API
/// wants it FLAT — `{"type":"function","name":N}` — so pull `.function.name`
/// up to the top level. Any object already flat (or otherwise shaped) passes
/// through unchanged.
fn flatten_tool_choice(tc: &Value) -> Value {
    if tc.get("type").and_then(Value::as_str) == Some("function") {
        if let Some(name) = tc.pointer("/function/name") {
            return json!({"type": "function", "name": name.clone()});
        }
    }
    tc.clone()
}

/// chat tool {type:function, function:{name,description,parameters}} ->
/// flat Responses {type:function, name, description?, parameters}.
fn flatten_tool(tool: &Value) -> Option<Value> {
    let f = tool.get("function")?;
    let name = f.get("name").and_then(Value::as_str)?;
    let mut out = json!({"type": "function", "name": name,
        "parameters": f.get("parameters").cloned()
            .unwrap_or_else(|| json!({"type": "object", "properties": {}}))});
    if let Some(desc) = f
        .get("description")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
    {
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
    /// tool_calls indices that received streamed argument deltas, so a
    /// complete-arguments item (on `.done`) isn't emitted a second time.
    tool_args_streamed: std::collections::HashSet<usize>,
    next_index: usize,
    has_tool_calls: bool,
    finished: bool,
    input_tokens: i64,
    output_tokens: i64,
}

impl ResponsesToOpenAiStream {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            tool_index: HashMap::new(),
            tool_args_streamed: std::collections::HashSet::new(),
            next_index: 0,
            has_tool_calls: false,
            finished: false,
            input_tokens: 0,
            output_tokens: 0,
        }
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
            "response.output_item.added" | "response.output_item.done" => {
                let item = data.get("item").cloned().unwrap_or(Value::Null);
                if item.get("type").and_then(Value::as_str) == Some("function_call") {
                    self.has_tool_calls = true;
                    // The args-delta events key by the ITEM id (`fc_…`); the
                    // downstream tool_use must carry the `call_id` (`call_…`) so
                    // a later tool_result links back. So index by item id but
                    // EMIT the call_id. (Keying by call_id drops every args
                    // delta — the "`path` is required" live failure.)
                    let item_id = item.get("id").and_then(Value::as_str);
                    let call_id = item.get("call_id").and_then(Value::as_str);
                    let key = item_id.or(call_id).unwrap_or("").to_string();
                    let emit_id = call_id.or(item_id).unwrap_or("").to_string();
                    let is_new = !self.tool_index.contains_key(&key);
                    let index = *self.tool_index.entry(key).or_insert_with(|| {
                        let i = self.next_index;
                        self.next_index += 1;
                        i
                    });
                    let name = item.get("name").and_then(Value::as_str).unwrap_or("");
                    if is_new {
                        out.push(self.chunk(
                            json!({"tool_calls": [{"index": index, "id": emit_id,
                            "type": "function", "function": {"name": name, "arguments": ""}}]}),
                            None,
                        ));
                    }
                    // Some responses carry the complete arguments string on the
                    // item itself (esp. on `.done`) rather than only via deltas.
                    // Emit it only if no deltas were streamed for this item, so
                    // we never double-count.
                    if let Some(args) = item
                        .get("arguments")
                        .and_then(Value::as_str)
                        .filter(|s| !s.is_empty())
                    {
                        if !self.tool_args_streamed.contains(&index) {
                            out.push(self.chunk(
                                json!({"tool_calls": [{"index": index,
                                "function": {"arguments": args}}]}),
                                None,
                            ));
                        }
                    }
                }
            }
            "response.function_call_arguments.delta" => {
                let id = data.get("item_id").and_then(Value::as_str).unwrap_or("");
                if let Some(&index) = self.tool_index.get(id) {
                    if let Some(delta) = data.get("delta").and_then(Value::as_str) {
                        self.tool_args_streamed.insert(index);
                        out.push(self.chunk(
                            json!({"tool_calls": [{"index": index,
                            "function": {"arguments": delta}}]}),
                            None,
                        ));
                    }
                }
            }
            "response.completed" | "response.incomplete" => {
                if let Some(u) = data.get("response").and_then(|r| r.get("usage")) {
                    self.input_tokens = u.get("input_tokens").and_then(Value::as_i64).unwrap_or(0);
                    self.output_tokens =
                        u.get("output_tokens").and_then(Value::as_i64).unwrap_or(0);
                }
                out.extend(self.terminal());
            }
            "response.failed" | "error" => {
                let msg = data
                    .pointer("/response/error/message")
                    .or_else(|| data.pointer("/error/message"))
                    .and_then(Value::as_str)
                    .unwrap_or("codex upstream error");
                out.push(self.chunk(json!({"content": ""}), None));
                out.push(json!({"error": {"message": msg}}));
                self.finished = true;
            }
            _ => {}
        }
        out
    }

    fn terminal(&mut self) -> Vec<Value> {
        if self.finished {
            return vec![];
        }
        self.finished = true;
        let finish = if self.has_tool_calls {
            "tool_calls"
        } else {
            "stop"
        };
        let mut c = self.chunk(json!({}), Some(finish));
        c["usage"] = json!({"prompt_tokens": self.input_tokens,
            "completion_tokens": self.output_tokens});
        vec![c]
    }

    /// Emit the terminal chunk if the stream closed (clean EOF) without a
    /// `response.completed`; a no-op if one was already emitted.
    pub fn finish(&mut self) -> Vec<Value> {
        self.terminal()
    }
    pub fn saw_terminal(&self) -> bool {
        self.finished
    }
    pub fn usage(&self) -> (i64, i64) {
        (self.input_tokens, self.output_tokens)
    }
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

/// The base model id Codex actually serves for a canonical review identity.
/// Legacy effort suffixes are parsed at the routing boundary, where provider
/// identity and the connection's real model catalog are available.
pub fn codex_base_model(model: &str) -> &str {
    model.strip_suffix("-review").unwrap_or(model)
}

/// Map the policy/UI effort identity to the Codex Responses wire value.
/// This is deliberately a protocol adapter, not a capability list.
pub fn reasoning_effort_for_request(effort: &str) -> &str {
    if effort == "ultra" {
        "max"
    } else {
        effort
    }
}

/// Apply an already-resolved native turn policy. Unlike shared Responses
/// normalization, this intentionally overwrites any translated effort.
pub(crate) fn apply_native_reasoning_effort(body: &mut Value, effort: &str) {
    let wire_effort = json!(reasoning_effort_for_request(effort));
    match body.get_mut("reasoning") {
        Some(Value::Object(reasoning)) => {
            reasoning.insert("effort".into(), wire_effort);
        }
        _ => body["reasoning"] = json!({"effort": wire_effort}),
    }
}

pub fn normalize_codex_responses_body(
    body: &mut Value,
    upstream_model: &str,
    explicit_effort: Option<&str>,
    cache_key: Option<&str>,
) {
    body["model"] = json!(codex_base_model(upstream_model));
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
        if let Some(effort) = body
            .get("reasoning_effort")
            .and_then(Value::as_str)
            .or(explicit_effort)
        {
            body["reasoning"] = json!({
                "effort": reasoning_effort_for_request(effort),
                "summary": "auto"
            });
        }
    }
    if let Some(reasoning) = body.get_mut("reasoning").and_then(Value::as_object_mut) {
        reasoning.entry("summary").or_insert_with(|| json!("auto"));
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
    fn response_failed_yields_an_error_element_for_the_pump_to_surface() {
        // `pump_codex` (client.rs) inspects each decoded element for an
        // `error.message` field BEFORE handing it to
        // `OpenAiToAnthropicStream::feed` (which doesn't understand this
        // shape and would otherwise silently drop it) and turns it into an
        // Anthropic error frame instead of swallowing it.
        let mut s = stream();
        let out = s.feed(
            "response.failed",
            &json!({"response": {"error": {"message": "rate limited"}}}),
        );
        let err = out
            .iter()
            .find(|el| el.get("error").is_some())
            .expect("response.failed must yield an error element");
        assert_eq!(err["error"]["message"], "rate limited");
        assert!(s.saw_terminal());
    }

    #[test]
    fn streamed_function_call_accumulates_and_finishes_as_tool_calls() {
        // Real Codex frames use a DISTINCT item id (`fc_1`) and `call_id`
        // (`call_1`): the item is announced with both, but the args-delta keys
        // by the item id. The decoder must index by item id yet emit the
        // call_id downstream — otherwise every args delta is dropped.
        let mut s = stream();
        let mut out = s.feed("response.output_item.added",
            &json!({"item": {"type": "function_call", "id": "fc_1", "call_id": "call_1", "name": "write"}}));
        out.extend(s.feed(
            "response.function_call_arguments.delta",
            &json!({"item_id": "fc_1", "delta": "{\"path\":"}),
        ));
        out.extend(s.feed(
            "response.function_call_arguments.delta",
            &json!({"item_id": "fc_1", "delta": "\"a.txt\"}"}),
        ));
        out.extend(s.feed(
            "response.completed",
            &json!({"response": {"usage": {"input_tokens": 12, "output_tokens": 7}}}),
        ));
        // Downstream tool_use carries the call_id (`call_1`), not the item id.
        let start = out
            .iter()
            .find(|c| c["choices"][0]["delta"]["tool_calls"][0]["id"] == "call_1")
            .unwrap();
        assert_eq!(
            start["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
            "write"
        );
        let args: String = out
            .iter()
            .filter_map(|c| {
                c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
            })
            .collect();
        assert_eq!(args, "{\"path\":\"a.txt\"}");
        assert_eq!(
            out.last().unwrap()["choices"][0]["finish_reason"],
            "tool_calls"
        );
        assert!(s.saw_terminal());
        assert_eq!(s.usage(), (12, 7));
    }

    #[test]
    fn complete_args_on_done_without_deltas_emit_open_and_args_once() {
        // A function_call whose COMPLETE arguments arrive on
        // `response.output_item.done` with NO `function_call_arguments.delta`
        // events must still emit the open tool_call (call_id + name) and
        // exactly ONE args chunk carrying the full arguments.
        let mut s = stream();
        let out = s.feed(
            "response.output_item.done",
            &json!({"item": {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                "name": "write", "arguments": "{\"path\":\"a.txt\"}"}}),
        );
        // Exactly one open chunk, carrying the call_id (not the item id) + name.
        let opens: Vec<&Value> = out
            .iter()
            .filter(|c| c["choices"][0]["delta"]["tool_calls"][0]["id"] == "call_1")
            .collect();
        assert_eq!(opens.len(), 1);
        assert_eq!(
            opens[0]["choices"][0]["delta"]["tool_calls"][0]["function"]["name"],
            "write"
        );
        // Exactly one args chunk, carrying the complete arguments verbatim.
        let arg_chunks: Vec<&str> = out
            .iter()
            .filter_map(|c| {
                c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
            })
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(arg_chunks, vec!["{\"path\":\"a.txt\"}"]);
    }

    #[test]
    fn done_args_are_suppressed_when_deltas_already_streamed() {
        // When arguments arrive BOTH via a streamed delta AND again on
        // `.done`, the delta path wins: the `.done` arguments are suppressed
        // by the `tool_args_streamed` guard so they are never double-counted.
        let mut s = stream();
        let mut out = s.feed(
            "response.output_item.added",
            &json!({"item": {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                "name": "write"}}),
        );
        out.extend(s.feed(
            "response.function_call_arguments.delta",
            &json!({"item_id": "fc_1", "delta": "{\"path\":\"a.txt\"}"}),
        ));
        out.extend(s.feed(
            "response.output_item.done",
            &json!({"item": {"type": "function_call", "id": "fc_1", "call_id": "call_1",
                "name": "write", "arguments": "{\"path\":\"a.txt\"}"}}),
        ));
        // The complete args string appears EXACTLY once across all chunks —
        // the `.done` copy was suppressed.
        let args: Vec<&str> = out
            .iter()
            .filter_map(|c| {
                c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"].as_str()
            })
            .filter(|s| !s.is_empty())
            .collect();
        assert_eq!(args, vec!["{\"path\":\"a.txt\"}"]);
    }

    #[test]
    fn tool_choice_string_passes_through_and_forced_tool_object_is_flattened() {
        // String forms pass through verbatim.
        let chat = json!({"model": "m", "messages": [], "tool_choice": "required"});
        let out = openai_chat_to_responses_request(&chat);
        assert_eq!(out["tool_choice"], "required");

        // A forced-tool object from anthropic_to_openai_request is the nested
        // OpenAI-chat shape {"type":"function","function":{"name":N}}; the
        // Responses API wants it FLAT {"type":"function","name":N}.
        let chat = json!({"model": "m", "messages": [],
            "tool_choice": {"type": "function", "function": {"name": "write"}}});
        let out = openai_chat_to_responses_request(&chat);
        assert_eq!(
            out["tool_choice"],
            json!({"type": "function", "name": "write"})
        );
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
        let fco = items
            .iter()
            .find(|i| i["type"] == "function_call_output")
            .unwrap();
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

        normalize_codex_responses_body(&mut body, "gpt-5.3-codex", Some("high"), Some("session-1"));

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
    fn codex_base_model_only_resolves_canonical_review_identity() {
        assert_eq!(codex_base_model("gpt-5.5-high"), "gpt-5.5-high");
        assert_eq!(
            codex_base_model("gpt-5.2-codex-xhigh"),
            "gpt-5.2-codex-xhigh"
        );
        assert_eq!(codex_base_model("gpt-5.5-review"), "gpt-5.5");
        assert_eq!(codex_base_model("gpt-5.5"), "gpt-5.5");
    }

    #[test]
    fn bare_effort_suffix_remains_exact_without_injected_effort() {
        let mut body = json!({"input": []});

        normalize_codex_responses_body(&mut body, "gpt-known-high", None, None);

        assert_eq!(body["model"], "gpt-known-high");
        assert!(body.get("reasoning").is_none());
    }

    #[test]
    fn codex_wire_maps_ultra_to_max_but_preserves_open_custom_values() {
        let mut ultra = json!({"input": []});
        normalize_codex_responses_body(&mut ultra, "gpt-5.5", Some("ultra"), None);
        assert_eq!(ultra["reasoning"]["effort"], "max");

        let mut custom = json!({"input": []});
        normalize_codex_responses_body(&mut custom, "gpt-5.5", Some("provider-experimental"), None);
        assert_eq!(custom["reasoning"]["effort"], "provider-experimental");
    }

    #[test]
    fn external_caller_reasoning_effort_wins_over_route_compatibility() {
        let mut body = json!({
            "input": [],
            "reasoning": {"effort": "low", "summary": "detailed"}
        });

        normalize_codex_responses_body(&mut body, "gpt-5.5", Some("ultra"), None);

        assert_eq!(body["reasoning"]["effort"], "low");
        assert_eq!(body["reasoning"]["summary"], "detailed");

        let mut flat = json!({"input": [], "reasoning_effort": "high"});
        normalize_codex_responses_body(&mut flat, "gpt-5.5", Some("low"), None);
        assert_eq!(flat["reasoning"]["effort"], "high");
    }

    #[test]
    fn malformed_reasoning_shape_does_not_panic() {
        let mut body = json!({"input": [], "reasoning": "malformed"});
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            normalize_codex_responses_body(&mut body, "gpt-5.5", Some("low"), None);
        }));
        assert!(result.is_ok());
        assert_eq!(body["reasoning"], "malformed");
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
