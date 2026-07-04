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
    // function-call items keyed by upstream tool_call index
    tools: std::collections::HashMap<i64, ToolItem>,
    tool_order: Vec<i64>,
    finish_reason: Option<String>,
}

struct ToolItem {
    output_index: i64,
    #[allow(dead_code)]
    call_id: String,
    #[allow(dead_code)]
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
            tools: std::collections::HashMap::new(),
            tool_order: Vec::new(),
            finish_reason: None,
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
        out.push(self.ev(
            "response.output_text.done",
            json!({"type": "response.output_text.done", "output_index": idx, "content_index": 0}),
        ));
        out.push(self.ev(
            "response.content_part.done",
            json!({"type": "response.content_part.done", "output_index": idx, "content_index": 0}),
        ));
        out.push(self.ev(
            "response.output_item.done",
            json!({"type": "response.output_item.done", "output_index": idx}),
        ));
    }

    pub fn feed(&mut self, chunk: &Value) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        self.ensure_started(chunk, &mut out);
        let delta = &chunk["choices"][0]["delta"];

        if let Some(text) = delta["content"].as_str() {
            if !text.is_empty() {
                self.open_message(&mut out);
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
            let starting = tc["id"].is_string() || tc["function"]["name"].is_string();
            if starting {
                // close the message before opening a tool item
                self.close_message(&mut out);
                let oidx = self.output_index;
                self.output_index += 1;
                let call_id = tc["id"].as_str().unwrap_or("").to_string();
                self.tools.insert(
                    tidx,
                    ToolItem {
                        output_index: oidx,
                        call_id: call_id.clone(),
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
                                 "call_id": call_id, "name": tc["function"]["name"], "arguments": ""}}),
                ));
            }
            if let Some(frag) = tc["function"]["arguments"].as_str() {
                if !frag.is_empty() {
                    if let Some(item) = self.tools.get(&tidx) {
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
            if let Some(item) = self.tools.get(&tidx) {
                let oidx = item.output_index;
                out.push(self.ev(
                    "response.function_call_arguments.done",
                    json!({"type": "response.function_call_arguments.done", "output_index": oidx}),
                ));
                out.push(self.ev(
                    "response.output_item.done",
                    json!({"type": "response.output_item.done", "output_index": oidx}),
                ));
            }
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
    }

    #[test]
    fn responses_error_frame_shape() {
        let s = ResponsesStreamState::new();
        let (name, data) = s.error_frame("boom");
        assert_eq!(name, "error");
        assert_eq!(data["message"], "boom");
    }
}
