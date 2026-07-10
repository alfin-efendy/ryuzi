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

/// Split a tool_result's content into (text-only content for the `tool`
/// message, hoisted OpenAI image parts). OpenAI's `role:"tool"` messages
/// cannot carry images, so vision content rides a synthetic user turn
/// placed right after the tool results.
fn split_tool_result_images(v: &Value) -> (Value, Vec<Value>) {
    let Some(blocks) = v.as_array() else {
        return (tool_content_text(v), Vec::new());
    };
    let mut images = Vec::new();
    for b in blocks {
        if b["type"] == "image" {
            let mt = b["source"]["media_type"].as_str().unwrap_or("image/png");
            let data = b["source"]["data"].as_str().unwrap_or("");
            images.push(json!({
                "type": "image_url",
                "image_url": {"url": format!("data:{mt};base64,{data}")}
            }));
        }
    }
    if images.is_empty() {
        return (tool_content_text(v), Vec::new());
    }
    let text: Vec<&str> = blocks
        .iter()
        .filter(|b| b["type"] == "text")
        .filter_map(|b| b["text"].as_str())
        .collect();
    (json!(text.join("\n")), images)
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
    // Streamed requests ask the OpenAI-format upstream for a final usage
    // frame so the router can capture real token counts (spec follow-up).
    if out.get("stream").and_then(Value::as_bool).unwrap_or(false) {
        out.insert("stream_options".into(), json!({"include_usage": true}));
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
                // Images from ALL tool_result blocks in this turn accumulate
                // here and hoist into a single user message after every
                // `role: tool` message — never interleaved between them (see
                // the contiguity note below).
                let mut hoisted_images: Vec<Value> = Vec::new();
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
                        "tool_result" => {
                            let (content, images) = split_tool_result_images(&b["content"]);
                            tool_results.push(json!({
                                "role": "tool",
                                "tool_call_id": b["tool_use_id"],
                                "content": content,
                            }));
                            hoisted_images.extend(images);
                        }
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
                } else {
                    // tool_result blocks must land immediately after the
                    // assistant tool_calls turn, before this turn's own user
                    // text — OpenAI-compat providers 400 on a `role: tool`
                    // message that doesn't directly follow it. All hoisted
                    // images from every tool_result in this turn are emitted
                    // as ONE user message right after the (contiguous) tool
                    // messages, never interleaved between them.
                    messages.append(&mut tool_results);
                    if !hoisted_images.is_empty() {
                        messages.push(json!({
                            "role": "user",
                            "content": std::mem::take(&mut hoisted_images),
                        }));
                    }
                    if !text_parts.is_empty() {
                        // Single text part → plain string; mixed parts stay array.
                        let only_text = text_parts.len() == 1 && text_parts[0]["type"] == "text";
                        let content = if only_text {
                            text_parts[0]["text"].clone()
                        } else {
                            Value::Array(text_parts)
                        };
                        messages.push(json!({"role": "user", "content": content}));
                    }
                }
                messages.extend(tool_results);
                // Defensive fallback: only reachable if tool_result blocks
                // somehow appeared in an assistant-role message, since the
                // non-assistant branch above already drains hoisted_images.
                if !hoisted_images.is_empty() {
                    messages.push(json!({"role": "user", "content": hoisted_images}));
                }
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
    // Anthropic extended-thinking → OpenAI reasoning_effort buckets. The
    // cache_control fields never survive translation: content blocks are
    // reconstructed field-by-field above.
    if let Some(budget) = body["thinking"]["budget_tokens"].as_i64() {
        let effort = if budget >= 16_384 {
            "high"
        } else if budget >= 8_192 {
            "medium"
        } else {
            "low"
        };
        out.insert("reasoning_effort".into(), json!(effort));
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
    let finish = resp["choices"][0]["finish_reason"]
        .as_str()
        .unwrap_or("stop");
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
    for b in resp["content"]
        .as_array()
        .cloned()
        .unwrap_or_default()
        .iter()
    {
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

// ---------------------------------------------------------------------------
// Streaming
// ---------------------------------------------------------------------------

enum OpenBlock {
    None,
    Text,
    Thinking,
    Tool,
}

/// Upstream OpenAI chunks → Anthropic SSE events (client called /v1/messages).
pub struct OpenAiToAnthropicStream {
    model: String,
    started: bool,
    block: OpenBlock,
    /// Anthropic content-block index (monotonic across text + tool blocks).
    index: i64,
    finish_reason: Option<String>,
    output_tokens: i64,
    input_tokens: i64,
    cache_read_tokens: i64,
    stopped: bool,
}

impl OpenAiToAnthropicStream {
    pub fn new(model: &str) -> Self {
        Self {
            model: model.to_string(),
            started: false,
            block: OpenBlock::None,
            index: -1,
            finish_reason: None,
            output_tokens: 0,
            input_tokens: 0,
            cache_read_tokens: 0,
            stopped: false,
        }
    }

    fn ensure_start(&mut self, chunk: &Value, out: &mut Vec<(String, Value)>) {
        if self.started {
            return;
        }
        self.started = true;
        let id = chunk["id"].as_str().unwrap_or("msg_router");
        out.push((
            "message_start".into(),
            json!({"type": "message_start", "message": {
                "id": id, "type": "message", "role": "assistant",
                "model": self.model, "content": [],
                "stop_reason": null, "stop_sequence": null,
                "usage": {"input_tokens": 0, "output_tokens": 0}
            }}),
        ));
    }

    fn close_block(&mut self, out: &mut Vec<(String, Value)>) {
        if !matches!(self.block, OpenBlock::None) {
            out.push((
                "content_block_stop".into(),
                json!({"type": "content_block_stop", "index": self.index}),
            ));
            self.block = OpenBlock::None;
        }
    }

    pub fn feed(&mut self, chunk: &Value) -> Vec<(String, Value)> {
        let mut out = Vec::new();
        self.ensure_start(chunk, &mut out);
        if let Some(u) = chunk.get("usage") {
            self.input_tokens = u["prompt_tokens"].as_i64().unwrap_or(self.input_tokens);
            self.output_tokens = u["completion_tokens"]
                .as_i64()
                .unwrap_or(self.output_tokens);
            self.cache_read_tokens = u["prompt_tokens_details"]["cached_tokens"]
                .as_i64()
                .unwrap_or(self.cache_read_tokens);
        }
        let choice = &chunk["choices"][0];
        let delta = &choice["delta"];

        // Reasoning (Codex/Kiro OpenAI-format upstreams stream it as
        // `reasoning_content`) surfaces as an Anthropic `thinking` block.
        // Placed BEFORE the text branch: reasoning typically precedes content,
        // so a turn that streams reasoning then text yields a thinking block
        // followed by a separate text block on the next index.
        if let Some(text) = delta["reasoning_content"].as_str() {
            if !text.is_empty() {
                if !matches!(self.block, OpenBlock::Thinking) {
                    self.close_block(&mut out);
                    self.index += 1;
                    self.block = OpenBlock::Thinking;
                    out.push((
                        "content_block_start".into(),
                        json!({"type": "content_block_start", "index": self.index,
                               "content_block": {"type": "thinking", "thinking": ""}}),
                    ));
                }
                out.push((
                    "content_block_delta".into(),
                    json!({"type": "content_block_delta", "index": self.index,
                           "delta": {"type": "thinking_delta", "thinking": text}}),
                ));
            }
        }
        if let Some(text) = delta["content"].as_str() {
            if !text.is_empty() {
                if !matches!(self.block, OpenBlock::Text) {
                    self.close_block(&mut out);
                    self.index += 1;
                    self.block = OpenBlock::Text;
                    out.push((
                        "content_block_start".into(),
                        json!({"type": "content_block_start", "index": self.index,
                               "content_block": {"type": "text", "text": ""}}),
                    ));
                }
                out.push((
                    "content_block_delta".into(),
                    json!({"type": "content_block_delta", "index": self.index,
                           "delta": {"type": "text_delta", "text": text}}),
                ));
            }
        }
        for tc in delta["tool_calls"].as_array().cloned().unwrap_or_default() {
            // A new tool call announces id+name; continuation carries args only.
            if tc["id"].is_string() || tc["function"]["name"].is_string() {
                self.close_block(&mut out);
                self.index += 1;
                self.block = OpenBlock::Tool;
                out.push((
                    "content_block_start".into(),
                    json!({"type": "content_block_start", "index": self.index,
                           "content_block": {"type": "tool_use",
                               "id": tc["id"], "name": tc["function"]["name"], "input": {}}}),
                ));
            }
            if let Some(frag) = tc["function"]["arguments"].as_str() {
                if !frag.is_empty() {
                    out.push((
                        "content_block_delta".into(),
                        json!({"type": "content_block_delta", "index": self.index,
                               "delta": {"type": "input_json_delta", "partial_json": frag}}),
                    ));
                }
            }
        }
        if let Some(fr) = choice["finish_reason"].as_str() {
            self.finish_reason = Some(fr.to_string());
        }
        out
    }

    pub fn finish(&mut self) -> Vec<(String, Value)> {
        if self.stopped {
            return vec![];
        }
        self.stopped = true;
        let mut out = Vec::new();
        self.close_block(&mut out);
        let stop = self
            .finish_reason
            .as_deref()
            .map(oai_finish_to_anthropic)
            .unwrap_or("end_turn");
        out.push((
            "message_delta".into(),
            json!({"type": "message_delta",
                   "delta": {"stop_reason": stop, "stop_sequence": null},
                   "usage": {"output_tokens": self.output_tokens,
                             "input_tokens": self.input_tokens,
                             "cache_read_input_tokens": self.cache_read_tokens}}),
        ));
        out.push(("message_stop".into(), json!({"type": "message_stop"})));
        out
    }

    /// Terminal error event in Anthropic SSE shape. Emit this INSTEAD of
    /// `finish()` when the upstream stream errored mid-flight; do not follow
    /// it with `message_stop`.
    pub fn error_frame(&self, message: &str) -> Vec<(String, Value)> {
        vec![(
            "error".into(),
            json!({"type": "error", "error": {"type": "api_error", "message": message}}),
        )]
    }

    /// Accumulated (input, output) token counts seen so far, from the
    /// upstream `usage` field carried on OpenAI chunks.
    pub fn usage(&self) -> (i64, i64) {
        (self.input_tokens, self.output_tokens)
    }

    /// True once a `finish_reason` chunk was observed. When the upstream
    /// stream ends cleanly WITHOUT ever seeing one, the caller must emit
    /// `error_frame` instead of `finish` — a clean EOF with no terminal event
    /// is a truncated stream, not a completed one.
    pub fn saw_terminal(&self) -> bool {
        self.finish_reason.is_some()
    }
}

/// Upstream Anthropic SSE events → OpenAI chunks (client called
/// /v1/chat/completions). Tool-call indices are renumbered 0..n in arrival
/// order because OpenAI indexes tool calls, not content blocks.
pub struct AnthropicToOpenAiStream {
    id: String,
    model: String,
    sent_role: bool,
    done: bool,
    /// anthropic block index → openai tool_call index
    tool_index: std::collections::HashMap<i64, i64>,
    next_tool: i64,
    finish: Option<String>,
    input_tokens: i64,
    usage_out: i64,
}

impl Default for AnthropicToOpenAiStream {
    fn default() -> Self {
        Self::new()
    }
}

impl AnthropicToOpenAiStream {
    pub fn new() -> Self {
        Self {
            id: "chatcmpl-router".into(),
            model: String::new(),
            sent_role: false,
            done: false,
            tool_index: Default::default(),
            next_tool: 0,
            finish: None,
            input_tokens: 0,
            usage_out: 0,
        }
    }

    fn chunk(&self, delta: Value, finish: Option<&str>) -> Value {
        json!({"id": self.id, "object": "chat.completion.chunk",
               "created": crate::paths::now_ms() / 1000, "model": self.model,
               "choices": [{"index": 0, "delta": delta,
                            "finish_reason": finish, "logprobs": null}]})
    }

    pub fn feed(&mut self, event: &str, data: &Value) -> Vec<Value> {
        let mut out = Vec::new();
        match event {
            "message_start" => {
                if let Some(id) = data["message"]["id"].as_str() {
                    self.id = id.to_string();
                }
                if let Some(m) = data["message"]["model"].as_str() {
                    self.model = m.to_string();
                }
                self.input_tokens = data["message"]["usage"]["input_tokens"]
                    .as_i64()
                    .unwrap_or(self.input_tokens);
                self.sent_role = true;
                out.push(self.chunk(json!({"role": "assistant", "content": ""}), None));
            }
            "content_block_start" => {
                let block = &data["content_block"];
                if block["type"] == "tool_use" {
                    let aidx = data["index"].as_i64().unwrap_or(0);
                    let oidx = self.next_tool;
                    self.next_tool += 1;
                    self.tool_index.insert(aidx, oidx);
                    out.push(self.chunk(
                        json!({"tool_calls": [{"index": oidx, "id": block["id"],
                               "type": "function",
                               "function": {"name": block["name"], "arguments": ""}}]}),
                        None,
                    ));
                }
            }
            "content_block_delta" => {
                let d = &data["delta"];
                match d["type"].as_str().unwrap_or("") {
                    "text_delta" => {
                        out.push(self.chunk(json!({"content": d["text"]}), None));
                    }
                    "input_json_delta" => {
                        let aidx = data["index"].as_i64().unwrap_or(0);
                        let oidx = *self.tool_index.get(&aidx).unwrap_or(&0);
                        out.push(self.chunk(
                            json!({"tool_calls": [{"index": oidx,
                                   "function": {"arguments": d["partial_json"]}}]}),
                            None,
                        ));
                    }
                    _ => {}
                }
            }
            "message_delta" => {
                if let Some(sr) = data["delta"]["stop_reason"].as_str() {
                    self.finish = Some(anthropic_stop_to_oai(sr).to_string());
                }
                self.usage_out = data["usage"]["output_tokens"]
                    .as_i64()
                    .unwrap_or(self.usage_out);
            }
            "message_stop" => {
                self.done = true;
                let finish = self.finish.clone().unwrap_or_else(|| "stop".into());
                out.push(self.chunk(json!({}), Some(&finish)));
            }
            _ => {} // ping, content_block_stop: nothing to emit
        }
        out
    }

    /// True once message_stop arrived — caller then emits `data: [DONE]`.
    pub fn finish(&self) -> bool {
        self.done
    }

    /// Terminal error chunk in OpenAI shape. Emit this INSTEAD of the normal
    /// finish chunk + `[DONE]` when the upstream stream errored mid-flight.
    pub fn error_frame(&self, message: &str) -> Value {
        json!({"error": {"message": message, "type": "api_error"}})
    }

    /// Accumulated (input, output) token counts seen so far: input from
    /// `message_start`'s usage, output from the terminal `message_delta`.
    pub fn usage(&self) -> (i64, i64) {
        (self.input_tokens, self.usage_out)
    }
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
            msgs[2]["tool_calls"][0]["function"]["arguments"]
                .as_str()
                .unwrap(),
            r#"{"city":"Jakarta"}"#
        );
        assert_eq!(
            msgs[3],
            json!({"role": "tool", "tool_call_id": "tu_1", "content": "sunny"})
        );
        assert_eq!(out["max_tokens"], 1024);
        assert_eq!(out["stop"], json!(["END"]));
        assert_eq!(
            out["tools"][0]["function"]["parameters"],
            json!({"type": "object"})
        );
        assert_eq!(out["tool_choice"], "auto");
        assert!(out.get("system").is_none());
        assert!(out.get("stop_sequences").is_none());
    }

    #[test]
    fn anthropic_stream_true_adds_openai_stream_options() {
        let req = json!({
            "model": "m", "max_tokens": 10, "stream": true,
            "messages": [{"role": "user", "content": "hi"}]
        });
        let out = anthropic_to_openai_request(&req).unwrap();
        assert_eq!(out["stream_options"], json!({"include_usage": true}));
    }

    #[test]
    fn request_translation_maps_thinking_to_reasoning_effort_and_drops_cache_control() {
        let body = json!({
            "model": "gpt-x",
            "system": [{"type":"text","text":"sys","cache_control":{"type":"ephemeral"}}],
            "thinking": {"type":"enabled","budget_tokens": 16384},
            "messages": [{"role":"user","content":[{"type":"text","text":"hi","cache_control":{"type":"ephemeral"}}]}],
            "max_tokens": 1000,
        });
        let out = anthropic_to_openai_request(&body).unwrap();
        assert_eq!(out["reasoning_effort"], "high");
        let s = serde_json::to_string(&out).unwrap();
        assert!(
            !s.contains("cache_control"),
            "cache_control must not reach OpenAI upstreams: {s}"
        );
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
        assert_eq!(
            out["content"][0],
            json!({"type": "text", "text": "checking"})
        );
        assert_eq!(out["content"][1]["type"], "tool_use");
        assert_eq!(out["content"][1]["input"], json!({"city": "Jakarta"}));
        assert_eq!(out["stop_reason"], "tool_use");
        assert_eq!(
            out["usage"],
            json!({"input_tokens": 10, "output_tokens": 5})
        );
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
        assert_eq!(
            out["usage"],
            json!({"prompt_tokens": 10, "completion_tokens": 5, "total_tokens": 15})
        );
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
    fn mixed_tool_result_and_text_turn_orders_tool_first() {
        let req = json!({
            "model": "m", "max_tokens": 10,
            "messages": [
                {"role": "user", "content": "what's the weather"},
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "tu_1", "name": "get_weather", "input": {"city": "Jakarta"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tu_1", "content": "sunny"},
                    {"type": "text", "text": "also note X"}
                ]}
            ]
        });
        let out = anthropic_to_openai_request(&req).unwrap();
        let msgs = out["messages"].as_array().unwrap();
        assert_eq!(msgs.len(), 4);
        assert_eq!(
            msgs[0],
            json!({"role": "user", "content": "what's the weather"})
        );
        assert_eq!(msgs[1]["role"], "assistant");
        assert_eq!(msgs[1]["tool_calls"][0]["id"], "tu_1");
        // The tool result must come immediately after the assistant tool_calls
        // turn, before the accompanying user text — OpenAI-compat providers
        // 400 on a `role: tool` message that doesn't directly follow it.
        assert_eq!(
            msgs[2],
            json!({"role": "tool", "tool_call_id": "tu_1", "content": "sunny"})
        );
        assert_eq!(msgs[3], json!({"role": "user", "content": "also note X"}));
    }

    #[test]
    fn tool_result_images_hoist_into_a_user_message() {
        let body = json!({
            "model": "m", "max_tokens": 10,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "tu_1", "name": "read", "input": {"path": "shot.png"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tu_1", "content": [
                        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}},
                        {"type": "text", "text": "[image shot.png attached]"}
                    ]}
                ]}
            ]
        });
        let out = anthropic_to_openai_request(&body).unwrap();
        let msgs = out["messages"].as_array().unwrap();
        // tool message keeps the text, image hoists into a following user turn
        let tool_msg = msgs.iter().find(|m| m["role"] == "tool").unwrap();
        assert_eq!(tool_msg["content"], "[image shot.png attached]");
        let user_msg = msgs.iter().find(|m| m["role"] == "user").unwrap();
        assert_eq!(user_msg["content"][0]["type"], "image_url");
        assert!(user_msg["content"][0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,"));
    }

    #[test]
    fn multiple_tool_result_images_hoist_into_one_contiguous_user_message() {
        // Two image-bearing tool_results in the SAME user turn must not
        // interleave role:"tool" messages with a hoisted role:"user" image
        // message in between — strict OpenAI-compat providers 400 on a
        // `role: tool` message that doesn't directly follow the assistant
        // tool_calls turn. Both images must hoist into a single user message
        // placed after both tool messages.
        let body = json!({
            "model": "m", "max_tokens": 10,
            "messages": [
                {"role": "assistant", "content": [
                    {"type": "tool_use", "id": "tu_1", "name": "read", "input": {"path": "shot1.png"}},
                    {"type": "tool_use", "id": "tu_2", "name": "read", "input": {"path": "shot2.png"}}
                ]},
                {"role": "user", "content": [
                    {"type": "tool_result", "tool_use_id": "tu_1", "content": [
                        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "QUJD"}},
                        {"type": "text", "text": "[image shot1.png attached]"}
                    ]},
                    {"type": "tool_result", "tool_use_id": "tu_2", "content": [
                        {"type": "image", "source": {"type": "base64", "media_type": "image/png", "data": "REVG"}},
                        {"type": "text", "text": "[image shot2.png attached]"}
                    ]}
                ]}
            ]
        });
        let out = anthropic_to_openai_request(&body).unwrap();
        let msgs = out["messages"].as_array().unwrap();

        let tool_positions: Vec<usize> = msgs
            .iter()
            .enumerate()
            .filter(|(_, m)| m["role"] == "tool")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            tool_positions.len(),
            2,
            "expected exactly two tool messages, got: {msgs:#?}"
        );
        assert_eq!(
            tool_positions[1],
            tool_positions[0] + 1,
            "tool messages must be contiguous (no message in between), got: {msgs:#?}"
        );

        let user_positions: Vec<usize> = msgs
            .iter()
            .enumerate()
            .filter(|(_, m)| m["role"] == "user")
            .map(|(i, _)| i)
            .collect();
        assert_eq!(
            user_positions.len(),
            1,
            "expected exactly one hoisted user image message, got: {msgs:#?}"
        );
        assert!(
            user_positions[0] > tool_positions[1],
            "hoisted user image message must come after both tool messages, got: {msgs:#?}"
        );

        let images = msgs[user_positions[0]]["content"].as_array().unwrap();
        assert_eq!(images.len(), 2, "both images must land in one message");
        assert_eq!(images[0]["type"], "image_url");
        assert!(images[0]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,QUJD"));
        assert_eq!(images[1]["type"], "image_url");
        assert!(images[1]["image_url"]["url"]
            .as_str()
            .unwrap()
            .starts_with("data:image/png;base64,REVG"));
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
        let calls = out["choices"][0]["message"]["tool_calls"]
            .as_array()
            .unwrap();
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

    #[test]
    fn openai_chunks_translate_to_anthropic_events() {
        let mut s = OpenAiToAnthropicStream::new("m");
        let mut evs = Vec::new();
        evs.extend(s.feed(&json!({"id": "c1", "choices": [{"index": 0, "delta": {"role": "assistant", "content": "He"}}]})));
        evs.extend(s.feed(&json!({"choices": [{"index": 0, "delta": {"content": "llo"}}]})));
        evs.extend(
            s.feed(&json!({"choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "id": "call_1", "function": {"name": "f", "arguments": "{\"a\""}}]}}]})),
        );
        evs.extend(
            s.feed(&json!({"choices": [{"index": 0, "delta": {"tool_calls": [
            {"index": 0, "function": {"arguments": ":1}"}}]}}]})),
        );
        evs.extend(s.feed(
            &json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "tool_calls"}],
                                  "usage": {"prompt_tokens": 7, "completion_tokens": 3}}),
        ));
        evs.extend(s.finish());
        let names: Vec<&str> = evs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "content_block_start",
                "content_block_delta",
                "content_block_delta",
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        assert_eq!(evs[1].1["content_block"]["type"], "text");
        assert_eq!(evs[2].1["delta"]["text"], "He");
        assert_eq!(evs[5].1["content_block"]["type"], "tool_use");
        assert_eq!(evs[5].1["content_block"]["name"], "f");
        assert_eq!(evs[6].1["delta"]["partial_json"], "{\"a\"");
        assert_eq!(evs[9].1["delta"]["stop_reason"], "tool_use");
        assert_eq!(evs[9].1["usage"]["output_tokens"], 3);
    }

    #[test]
    fn reasoning_content_chunk_yields_thinking_block() {
        // Codex/Kiro OpenAI-format upstreams stream reasoning as
        // `delta.reasoning_content`; it must surface as an Anthropic
        // `thinking` block (thinking-typed content_block_start + a
        // thinking_delta) so the native runner shows the model's reasoning
        // rather than silently dropping it. Reasoning precedes text, so the
        // thinking block gets its own (earlier) index and the text lands in a
        // separate, later block.
        let mut s = OpenAiToAnthropicStream::new("m");
        let mut evs = Vec::new();
        evs.extend(s.feed(&json!({"id": "c1", "choices": [{"index": 0,
            "delta": {"reasoning_content": "let me think"}}]})));
        evs.extend(s.feed(&json!({"choices": [{"index": 0,
            "delta": {"reasoning_content": " more"}}]})));
        evs.extend(s.feed(&json!({"choices": [{"index": 0,
            "delta": {"content": "answer"}}]})));
        evs.extend(
            s.feed(&json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]})),
        );
        evs.extend(s.finish());
        let names: Vec<&str> = evs.iter().map(|(n, _)| n.as_str()).collect();
        assert_eq!(
            names,
            vec![
                "message_start",
                "content_block_start", // thinking block opens
                "content_block_delta", // thinking_delta
                "content_block_delta", // thinking_delta (continuation)
                "content_block_stop",  // thinking block closes
                "content_block_start", // text block opens (separate index)
                "content_block_delta", // text_delta
                "content_block_stop",
                "message_delta",
                "message_stop",
            ]
        );
        // Thinking block: index 0, correct type + delta shape (this is what
        // MessageStreamEvent::from_event decodes as ThinkingDelta).
        assert_eq!(evs[1].1["content_block"]["type"], "thinking");
        assert_eq!(evs[1].1["index"], 0);
        assert_eq!(evs[2].1["delta"]["type"], "thinking_delta");
        assert_eq!(evs[2].1["delta"]["thinking"], "let me think");
        assert_eq!(evs[2].1["index"], 0);
        assert_eq!(evs[3].1["delta"]["thinking"], " more");
        // Text block lands on a separate, later index.
        assert_eq!(evs[5].1["content_block"]["type"], "text");
        assert_eq!(evs[5].1["index"], 1);
        assert_eq!(evs[6].1["delta"]["text"], "answer");
        assert_eq!(evs[6].1["index"], 1);
    }

    #[test]
    fn openai_to_anthropic_stream_saw_terminal_tracks_finish_reason() {
        let mut s = OpenAiToAnthropicStream::new("m");
        assert!(!s.saw_terminal());
        s.feed(&json!({"id": "c1", "choices": [{"index": 0, "delta": {"content": "hi"}}]}));
        assert!(!s.saw_terminal(), "content deltas alone aren't terminal");
        s.feed(&json!({"choices": [{"index": 0, "delta": {}, "finish_reason": "stop"}]}));
        assert!(s.saw_terminal());
    }

    #[test]
    fn anthropic_events_translate_to_openai_chunks() {
        let mut s = AnthropicToOpenAiStream::new();
        let mut chunks = Vec::new();
        chunks.extend(s.feed(
            "message_start",
            &json!({"message": {"id": "msg_1", "model": "m"}}),
        ));
        chunks.extend(s.feed(
            "content_block_start",
            &json!({"index": 0, "content_block": {"type": "text", "text": ""}}),
        ));
        chunks.extend(s.feed(
            "content_block_delta",
            &json!({"index": 0, "delta": {"type": "text_delta", "text": "Hi"}}),
        ));
        chunks.extend(s.feed("content_block_stop", &json!({"index": 0})));
        chunks.extend(s.feed(
            "content_block_start",
            &json!({"index": 1, "content_block": {"type": "tool_use", "id": "tu_1", "name": "f"}}),
        ));
        chunks.extend(s.feed(
            "content_block_delta",
            &json!({"index": 1, "delta": {"type": "input_json_delta", "partial_json": "{}"}}),
        ));
        chunks.extend(s.feed(
            "message_delta",
            &json!({"delta": {"stop_reason": "tool_use"}, "usage": {"output_tokens": 3}}),
        ));
        chunks.extend(s.feed("message_stop", &json!({})));
        assert!(s.finish());
        // role announcement
        assert_eq!(chunks[0]["choices"][0]["delta"]["role"], "assistant");
        // text delta
        assert!(chunks
            .iter()
            .any(|c| c["choices"][0]["delta"]["content"] == "Hi"));
        // tool call start carries id+name, later fragment carries arguments
        assert!(chunks
            .iter()
            .any(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["name"] == "f"));
        assert!(chunks
            .iter()
            .any(|c| c["choices"][0]["delta"]["tool_calls"][0]["function"]["arguments"] == "{}"));
        // finish chunk
        let last = chunks.last().unwrap();
        assert_eq!(last["choices"][0]["finish_reason"], "tool_calls");
    }

    #[test]
    fn openai_to_anthropic_error_frame_is_anthropic_shaped() {
        let s = OpenAiToAnthropicStream::new("m");
        let ev = s.error_frame("upstream stream interrupted: boom");
        assert_eq!(ev.len(), 1);
        assert_eq!(ev[0].0, "error");
        assert_eq!(ev[0].1["type"], "error");
        assert_eq!(ev[0].1["error"]["type"], "api_error");
        assert_eq!(
            ev[0].1["error"]["message"],
            "upstream stream interrupted: boom"
        );
    }

    #[test]
    fn anthropic_to_openai_error_frame_is_openai_shaped() {
        let s = AnthropicToOpenAiStream::new();
        let chunk = s.error_frame("upstream stream interrupted: boom");
        assert_eq!(
            chunk["error"]["message"],
            "upstream stream interrupted: boom"
        );
        assert_eq!(chunk["error"]["type"], "api_error");
    }

    #[test]
    fn finish_emits_input_and_cache_tokens_in_terminal_message_delta() {
        let mut tr = OpenAiToAnthropicStream::new("gpt-x");
        tr.feed(&json!({
            "id": "c1",
            "choices": [{"delta": {"content": "hi"}, "finish_reason": "stop"}],
            "usage": {
                "prompt_tokens": 1200,
                "completion_tokens": 7,
                "prompt_tokens_details": {"cached_tokens": 900}
            }
        }));
        let out = tr.finish();
        let delta = out
            .iter()
            .find(|(name, _)| name == "message_delta")
            .expect("a message_delta event");
        assert_eq!(delta.1["usage"]["output_tokens"], 7);
        assert_eq!(delta.1["usage"]["input_tokens"], 1200);
        assert_eq!(delta.1["usage"]["cache_read_input_tokens"], 900);
    }
}
