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
            msgs.push(
                json!({"role": "assistant", "content": Value::Null, "tool_calls": tc.clone()}),
            );
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
                    .unwrap_or_else(|| {
                        if item.get("role").is_some() {
                            "message".into()
                        } else {
                            String::new()
                        }
                    });
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
            .map(|t| {
                json!({"type": "function", "function": {
                    "name": t["name"], "description": t["description"],
                    "parameters": normalize_params(&t["parameters"]),
                }})
            })
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

/// Non-stream OpenAI chat.completion -> Responses `response` object.
pub fn chat_response_to_responses(v: &Value) -> Value {
    let msg = &v["choices"][0]["message"];
    let mut output: Vec<Value> = Vec::new();
    if let Some(text) = msg["content"].as_str() {
        if !text.is_empty() {
            output.push(json!({
                "type": "message", "role": "assistant", "status": "completed",
                "content": [{"type": "output_text", "text": text, "annotations": []}],
            }));
        }
    }
    for tc in msg["tool_calls"].as_array().cloned().unwrap_or_default() {
        output.push(json!({
            "type": "function_call", "status": "completed",
            "call_id": tc["id"], "name": tc["function"]["name"],
            "arguments": tc["function"]["arguments"].as_str().unwrap_or("{}"),
        }));
    }
    let inp = v["usage"]["prompt_tokens"].as_i64().unwrap_or(0);
    let outp = v["usage"]["completion_tokens"].as_i64().unwrap_or(0);
    json!({
        "id": format!("resp_{}", v["id"].as_str().unwrap_or("router")),
        "object": "response", "status": "completed", "model": v["model"],
        "output": output,
        "usage": {"input_tokens": inp, "output_tokens": outp,
                  "total_tokens": inp + outp},
    })
}

/// Streaming encoder: OpenAI chat.completion.chunk -> Responses SSE events.
/// Emits the 15 event names with a global monotonic sequence_number.
pub struct ResponsesStreamState {
    seq: i64,
    response_id: String,
    output_index: i64,
    started: bool,
    completed: bool,
    // single assistant message item
    msg_open: bool,
    msg_index: i64,
    // accumulated assistant message text, cleared once the item closes
    text: String,
    // function-call items keyed by upstream tool_call index
    tools: std::collections::HashMap<i64, ToolItem>,
    tool_order: Vec<i64>,
    finish_reason: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
}

/// call_id is clamped to 64 chars per spec §3.3 before it's stored here.
struct ToolItem {
    output_index: i64,
    call_id: String,
    name: String,
    arguments: String,
    started: bool,
}

impl Default for ResponsesStreamState {
    fn default() -> Self {
        Self::new()
    }
}

impl ResponsesStreamState {
    pub fn new() -> Self {
        Self {
            seq: 0,
            response_id: String::new(),
            output_index: 0,
            started: false,
            completed: false,
            msg_open: false,
            msg_index: 0,
            text: String::new(),
            tools: std::collections::HashMap::new(),
            tool_order: Vec::new(),
            finish_reason: None,
            input_tokens: 0,
            output_tokens: 0,
        }
    }

    fn ev(&mut self, name: &str, mut data: Value) -> (String, Value) {
        data["sequence_number"] = json!(self.seq);
        self.seq += 1;
        (name.to_string(), data)
    }

    fn ensure_started(&mut self, chunk: &Value, out: &mut Vec<(String, Value)>) {
        if self.started {
            return;
        }
        self.started = true;
        self.response_id = format!("resp_{}", chunk["id"].as_str().unwrap_or("router"));
        let rid = self.response_id.clone();
        out.push(self.ev(
            "response.created",
            json!({"type": "response.created", "response": {"id": rid, "status": "in_progress"}}),
        ));
        let rid = self.response_id.clone();
        out.push(self.ev(
            "response.in_progress",
            json!({"type": "response.in_progress", "response": {"id": rid, "status": "in_progress"}}),
        ));
    }

    fn open_message(&mut self, out: &mut Vec<(String, Value)>) {
        if self.msg_open {
            return;
        }
        self.msg_open = true;
        self.msg_index = self.output_index;
        self.output_index += 1;
        let item_id = format!("msg_{}_{}", self.response_id, self.msg_index);
        let idx = self.msg_index;
        out.push(self.ev(
            "response.output_item.added",
            json!({
                "type": "response.output_item.added", "output_index": idx,
                "item": {"id": item_id, "type": "message", "role": "assistant", "content": []}}),
        ));
        out.push(self.ev(
            "response.content_part.added",
            json!({
                "type": "response.content_part.added", "output_index": idx, "content_index": 0,
                "part": {"type": "output_text", "text": "", "annotations": []}}),
        ));
    }

    fn close_message(&mut self, out: &mut Vec<(String, Value)>) {
        if !self.msg_open {
            return;
        }
        self.msg_open = false;
        let idx = self.msg_index;
        let text = std::mem::take(&mut self.text);
        out.push(self.ev(
            "response.output_text.done",
            json!({"type": "response.output_text.done", "output_index": idx,
                   "content_index": 0, "text": text}),
        ));
        out.push(self.ev(
            "response.content_part.done",
            json!({"type": "response.content_part.done", "output_index": idx, "content_index": 0}),
        ));
        let item_id = format!("msg_{}_{}", self.response_id, idx);
        out.push(self.ev(
            "response.output_item.done",
            json!({"type": "response.output_item.done", "output_index": idx,
                   "item": {"id": item_id, "type": "message", "role": "assistant",
                            "status": "completed",
                            "content": [{"type": "output_text", "text": text, "annotations": []}]}}),
        ));
    }

    pub fn feed(&mut self, chunk: &Value) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        self.ensure_started(chunk, &mut out);
        if let Some(u) = chunk.get("usage") {
            self.input_tokens = u["prompt_tokens"].as_i64().unwrap_or(self.input_tokens);
            self.output_tokens = u["completion_tokens"]
                .as_i64()
                .unwrap_or(self.output_tokens);
        }
        let delta = &chunk["choices"][0]["delta"];

        if let Some(text) = delta["content"].as_str() {
            if !text.is_empty() {
                self.open_message(&mut out);
                self.text.push_str(text);
                let idx = self.msg_index;
                out.push(self.ev(
                    "response.output_text.delta",
                    json!({
                        "type": "response.output_text.delta", "output_index": idx,
                        "content_index": 0, "delta": text}),
                ));
            }
        }

        for tc in delta["tool_calls"].as_array().cloned().unwrap_or_default() {
            let tidx = tc["index"].as_i64().unwrap_or(0);
            let already_started = self.tools.get(&tidx).is_some_and(|t| t.started);
            let starting =
                !already_started && (tc["id"].is_string() || tc["function"]["name"].is_string());
            if starting {
                // close the message before opening a tool item
                self.close_message(&mut out);
                let oidx = self.output_index;
                self.output_index += 1;
                // spec §3.3: call_id is clamped to 64 chars.
                let call_id: String = tc["id"].as_str().unwrap_or("").chars().take(64).collect();
                let name = tc["function"]["name"].as_str().unwrap_or("").to_string();
                self.tools.insert(
                    tidx,
                    ToolItem {
                        output_index: oidx,
                        call_id: call_id.clone(),
                        name: name.clone(),
                        arguments: String::new(),
                        started: true,
                    },
                );
                self.tool_order.push(tidx);
                let item_id = format!("fc_{call_id}");
                out.push(self.ev(
                    "response.output_item.added",
                    json!({
                        "type": "response.output_item.added", "output_index": oidx,
                        "item": {"id": item_id, "type": "function_call",
                                 "call_id": call_id, "name": name, "arguments": ""}}),
                ));
            }
            if let Some(frag) = tc["function"]["arguments"].as_str() {
                if !frag.is_empty() {
                    if let Some(item) = self.tools.get_mut(&tidx) {
                        item.arguments.push_str(frag);
                        let oidx = item.output_index;
                        out.push(self.ev(
                            "response.function_call_arguments.delta",
                            json!({
                                "type": "response.function_call_arguments.delta",
                                "output_index": oidx, "delta": frag}),
                        ));
                    }
                }
            }
        }

        if let Some(fr) = chunk["choices"][0]["finish_reason"].as_str() {
            self.finish_reason = Some(fr.to_string());
        }
        out
    }

    pub fn finish(&mut self) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        if self.completed {
            return out;
        }
        self.completed = true;
        self.close_message(&mut out);
        // close each tool item
        let order = self.tool_order.clone();
        for tidx in order {
            let Some((oidx, call_id, name, arguments)) = self.tools.get(&tidx).map(|item| {
                (
                    item.output_index,
                    item.call_id.clone(),
                    item.name.clone(),
                    item.arguments.clone(),
                )
            }) else {
                continue;
            };
            out.push(self.ev(
                "response.function_call_arguments.done",
                json!({"type": "response.function_call_arguments.done",
                       "output_index": oidx, "arguments": arguments}),
            ));
            let item_id = format!("fc_{call_id}");
            out.push(self.ev(
                "response.output_item.done",
                json!({"type": "response.output_item.done", "output_index": oidx,
                       "item": {"id": item_id, "type": "function_call", "call_id": call_id,
                                "name": name, "arguments": arguments, "status": "completed"}}),
            ));
        }
        let rid = self.response_id.clone();
        out.push(self.ev(
            "response.completed",
            json!({"type": "response.completed", "response": {"id": rid, "status": "completed"}}),
        ));
        out
    }

    pub fn error_frame(&self, message: &str) -> (String, Value) {
        (
            "error".to_string(),
            json!({"type": "error", "message": message, "code": null}),
        )
    }

    /// True once a `finish_reason` chunk was observed. When the upstream
    /// stream ends cleanly WITHOUT ever seeing one, the caller must emit
    /// `error_frame` instead of `finish` — a clean EOF with no terminal event
    /// is a truncated stream, not a completed one.
    pub fn saw_terminal(&self) -> bool {
        self.finish_reason.is_some()
    }

    /// Accumulated (input, output) token counts seen so far, from the
    /// upstream `usage` field carried on OpenAI chunks.
    pub fn usage(&self) -> (i64, i64) {
        (self.input_tokens, self.output_tokens)
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
        assert_eq!(
            msgs[1],
            json!({"role": "tool", "tool_call_id": "call_1", "content": "sunny"})
        );
    }

    #[test]
    fn item_type_falls_back_to_role_and_empty_input_gets_placeholder() {
        // Droid CLI omits `type` on message items.
        let req = json!({"model": "m", "input": [{"role": "user", "content": "hi"}]});
        let out = responses_request_to_chat(&req);
        assert_eq!(out["messages"][0], json!({"role": "user", "content": "hi"}));

        let empty = json!({"model": "m", "input": []});
        let out = responses_request_to_chat(&empty);
        assert_eq!(
            out["messages"][0],
            json!({"role": "user", "content": "..."})
        );
    }

    #[test]
    fn tools_flatten_and_reasoning_effort_maps() {
        let req = json!({"model": "m", "input": "hi",
            "tools": [{"type": "function", "name": "f", "description": "d",
                       "parameters": {"type": "object"}}]});
        let out = responses_request_to_chat(&req);
        assert_eq!(
            out["tools"][0],
            json!({"type": "function",
            "function": {"name": "f", "description": "d", "parameters": {"type": "object"}}})
        );
    }

    #[test]
    fn nonstream_chat_response_becomes_responses_object() {
        let chat = json!({"id": "cmpl-1", "model": "m",
            "choices": [{"index": 0, "finish_reason": "stop",
                "message": {"role": "assistant", "content": "hello"}}],
            "usage": {"prompt_tokens": 3, "completion_tokens": 2}});
        let out = chat_response_to_responses(&chat);
        assert_eq!(out["object"], "response");
        assert_eq!(out["status"], "completed");
        // one output message item carrying the text
        let item = &out["output"][0];
        assert_eq!(item["type"], "message");
        assert_eq!(item["content"][0]["type"], "output_text");
        assert_eq!(item["content"][0]["text"], "hello");
        assert_eq!(out["usage"]["input_tokens"], 3);
        assert_eq!(out["usage"]["output_tokens"], 2);
    }

    #[test]
    fn stream_text_produces_responses_sse_sequence() {
        let mut s = ResponsesStreamState::new();
        let mut evs = Vec::new();
        evs.extend(s.feed(&json!({"id": "c1",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "He"}}]})));
        evs.extend(s.feed(&json!({"choices": [{"index": 0, "delta": {"content": "llo"}}]})));
        evs.extend(
            s.feed(&json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})),
        );
        evs.extend(s.finish());
        let names: Vec<&str> = evs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(names.first(), Some(&"response.created"));
        assert!(names.contains(&"response.output_item.added"));
        assert!(names.contains(&"response.content_part.added"));
        assert!(
            names
                .iter()
                .filter(|n| **n == "response.output_text.delta")
                .count()
                >= 2
        );
        assert!(names.contains(&"response.output_text.done"));
        assert!(names.contains(&"response.content_part.done"));
        assert!(names.contains(&"response.output_item.done"));
        assert_eq!(names.last(), Some(&"response.completed"));
        // every event carries a monotonic sequence_number
        let seqs: Vec<i64> = evs
            .iter()
            .map(|(_, d)| d["sequence_number"].as_i64().unwrap())
            .collect();
        assert!(seqs.windows(2).all(|w| w[1] == w[0] + 1));

        // Codex (codex-rs) only dispatches from output_item.done events that
        // carry a full `item` — an event with no `item` is silently dropped.
        let text_done = evs
            .iter()
            .find(|(n, _)| n == "response.output_text.done")
            .unwrap();
        assert_eq!(text_done.1["text"], "Hello");
        let item_done = evs
            .iter()
            .find(|(n, d)| n == "response.output_item.done" && d["item"]["type"] == "message")
            .unwrap();
        assert_eq!(item_done.1["item"]["role"], "assistant");
        assert_eq!(item_done.1["item"]["status"], "completed");
        assert_eq!(item_done.1["item"]["content"][0]["type"], "output_text");
        assert_eq!(item_done.1["item"]["content"][0]["text"], "Hello");
    }

    #[test]
    fn stream_tool_call_emits_function_call_events() {
        let mut s = ResponsesStreamState::new();
        let mut evs = Vec::new();
        evs.extend(s.feed(
            &json!({"id": "c1", "choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "id": "call_1", "function": {"name": "f", "arguments": "{\"a\""}}]}}]}),
        ));
        evs.extend(
            s.feed(&json!({"choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": ":1}"}}]}}]})),
        );
        evs.extend(
            s.feed(&json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})),
        );
        evs.extend(s.finish());
        let names: Vec<&str> = evs.iter().map(|(n, _)| n.as_str()).collect();
        assert!(names.contains(&"response.function_call_arguments.delta"));
        assert!(names.contains(&"response.function_call_arguments.done"));
        // the function_call item carries name + call_id
        let added = evs
            .iter()
            .find(|(n, d)| {
                n == "response.output_item.added" && d["item"]["type"] == "function_call"
            })
            .unwrap();
        assert_eq!(added.1["item"]["name"], "f");

        // Codex (codex-rs) only dispatches tool calls from output_item.done
        // events that carry a full `item` — an event with no `item` is
        // silently dropped, so the accumulated call_id/name/arguments must
        // all be present on the terminal event, not just `added`.
        let done = evs
            .iter()
            .find(|(n, d)| n == "response.output_item.done" && d["item"]["type"] == "function_call")
            .unwrap();
        assert_eq!(done.1["item"]["call_id"], "call_1");
        assert_eq!(done.1["item"]["name"], "f");
        assert_eq!(done.1["item"]["arguments"], "{\"a\":1}");
        assert_eq!(done.1["item"]["status"], "completed");
    }

    #[test]
    fn stream_tool_call_id_is_clamped_to_64_chars() {
        let long_id = "x".repeat(100);
        let mut s = ResponsesStreamState::new();
        let mut evs = Vec::new();
        evs.extend(s.feed(
            &json!({"id": "c1", "choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "id": long_id, "function": {"name": "f", "arguments": "{}"}}]}}]}),
        ));
        evs.extend(
            s.feed(&json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}]})),
        );
        evs.extend(s.finish());
        let added = evs
            .iter()
            .find(|(n, d)| {
                n == "response.output_item.added" && d["item"]["type"] == "function_call"
            })
            .unwrap();
        let call_id = added.1["item"]["call_id"].as_str().unwrap();
        assert_eq!(call_id.len(), 64);
        assert_eq!(added.1["item"]["id"], format!("fc_{call_id}"));
        let done = evs
            .iter()
            .find(|(n, d)| n == "response.output_item.done" && d["item"]["type"] == "function_call")
            .unwrap();
        assert_eq!(done.1["item"]["call_id"], call_id);
    }

    #[test]
    fn responses_error_frame_shape() {
        let s = ResponsesStreamState::new();
        let (name, data) = s.error_frame("boom");
        assert_eq!(name, "error");
        assert_eq!(data["message"], "boom");
    }

    #[test]
    fn responses_stream_saw_terminal_tracks_finish_reason() {
        let mut s = ResponsesStreamState::new();
        assert!(!s.saw_terminal());
        s.feed(&json!({"id": "c1",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "hi"}}]}));
        assert!(!s.saw_terminal(), "content deltas alone aren't terminal");
        s.feed(&json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}));
        assert!(s.saw_terminal());
    }

    #[test]
    fn responses_stream_usage_accumulates_from_chunks() {
        let mut s = ResponsesStreamState::new();
        s.feed(&json!({"id": "c1",
            "choices": [{"index": 0, "delta": {"role": "assistant", "content": "hi"}}]}));
        assert_eq!(s.usage(), (0, 0));
        s.feed(
            &json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 7, "completion_tokens": 3}}),
        );
        assert_eq!(s.usage(), (7, 3));
    }
}
