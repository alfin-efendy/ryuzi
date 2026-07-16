//! Lightweight model capability inference for routing.
use serde_json::Value;

use crate::harness::native::capabilities::TransportToolCapabilities;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ModelCapabilities {
    pub vision: bool,
    pub pdf: bool,
    pub audio: bool,
    pub image_generation: bool,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RequiredCapabilities {
    pub vision: bool,
    pub pdf: bool,
    pub audio: bool,
    pub image_generation: bool,
}

/// Tool-wire features frozen into a request. Unlike multimodal preferences,
/// these are hard requirements: an incompatible retry target must not receive
/// the request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ToolTransportRequirements {
    pub function_tools: bool,
    pub custom_freeform_tools: bool,
    pub strict_function_schema: bool,
    pub tool_output_schema: bool,
}

impl ToolTransportRequirements {
    pub fn satisfied_by(self, capabilities: TransportToolCapabilities) -> bool {
        (!self.function_tools || capabilities.supports_function_tools)
            && (!self.custom_freeform_tools || capabilities.supports_custom_freeform_tools)
            && (!self.strict_function_schema || capabilities.supports_strict_function_schema)
            && (!self.tool_output_schema || capabilities.supports_tool_output_schema)
    }
}

pub fn tool_transport_requirements_from_body(body: &Value) -> ToolTransportRequirements {
    let mut requirements = ToolTransportRequirements::default();
    let Some(tools) = body.get("tools").and_then(Value::as_array) else {
        return requirements;
    };
    for tool in tools {
        let tool_type = tool.get("type").and_then(Value::as_str);
        let function = tool.get("function").unwrap_or(tool);
        let custom = matches!(tool_type, Some("custom" | "custom_tool" | "freeform"));
        requirements.custom_freeform_tools |= custom;
        requirements.function_tools |= !custom;
        requirements.strict_function_schema |= function
            .get("strict")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        requirements.tool_output_schema |= has_non_null(tool, "output_schema")
            || has_non_null(function, "output_schema")
            || tool
                .get("custom")
                .is_some_and(|custom| has_non_null(custom, "output_schema"));
    }
    requirements
}

fn has_non_null(value: &Value, key: &str) -> bool {
    value.get(key).is_some_and(|value| !value.is_null())
}

impl RequiredCapabilities {
    pub fn any(self) -> bool {
        self.vision || self.pdf || self.audio || self.image_generation
    }

    pub fn satisfied_by(self, caps: ModelCapabilities) -> bool {
        (!self.vision || caps.vision)
            && (!self.pdf || caps.pdf)
            && (!self.audio || caps.audio)
            && (!self.image_generation || caps.image_generation)
    }
}

pub fn model_capabilities(model: &str) -> ModelCapabilities {
    let m = model.to_ascii_lowercase();
    let vision = contains_any(
        &m,
        &[
            "gpt-4o", "gpt-4.1", "gpt-5", "o3", "o4", "o5", "claude", "gemini", "vision",
            "pixtral", "llava", "qwen-vl", "qwen2-vl", "qwen3-vl", "grok-4",
        ],
    );
    let audio = contains_any(
        &m,
        &[
            "audio",
            "realtime",
            "transcribe",
            "transcription",
            "whisper",
            "tts",
            "gpt-4o",
        ],
    );
    let image_generation = contains_any(&m, &["gpt-image", "dall-e", "imagen", "image-generation"])
        || (m.contains("image") && !m.contains("image_url"));
    ModelCapabilities {
        vision,
        pdf: vision || contains_any(&m, &["pdf", "document"]),
        audio,
        image_generation,
    }
}

pub fn required_capabilities_from_body(body: &Value) -> RequiredCapabilities {
    let mut required = RequiredCapabilities::default();
    scan_value(body, &mut required);
    required
}

fn scan_value(value: &Value, required: &mut RequiredCapabilities) {
    match value {
        Value::Object(map) => {
            for (key, value) in map {
                scan_text(&key.to_ascii_lowercase(), required);
                scan_value(value, required);
            }
        }
        Value::Array(items) => {
            for item in items {
                scan_value(item, required);
            }
        }
        Value::String(s) => scan_text(&s.to_ascii_lowercase(), required),
        _ => {}
    }
}

fn scan_text(text: &str, required: &mut RequiredCapabilities) {
    if contains_any(
        text,
        &[
            "image_url",
            "input_image",
            "data:image",
            "media_type:image",
            "image/",
        ],
    ) {
        required.vision = true;
    }
    if contains_any(
        text,
        &[
            "application/pdf",
            "data:application/pdf",
            "input_file",
            "file_data",
            ".pdf",
        ],
    ) {
        required.pdf = true;
        required.vision = true;
    }
    if contains_any(
        text,
        &[
            "input_audio",
            "data:audio",
            "audio/",
            "transcription",
            "transcribe",
        ],
    ) {
        required.audio = true;
    }
    if contains_any(text, &["image_generation", "gpt-image", "generate_image"]) {
        required.image_generation = true;
    }
}

fn contains_any(haystack: &str, needles: &[&str]) -> bool {
    needles.iter().any(|needle| haystack.contains(needle))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn detects_image_inputs_from_openai_chat_body() {
        let body = json!({
            "messages": [{
                "role": "user",
                "content": [{"type": "image_url", "image_url": {"url": "data:image/png;base64,abc"}}]
            }]
        });

        let required = required_capabilities_from_body(&body);

        assert!(required.vision);
        assert!(!required.audio);
    }

    #[test]
    fn infers_common_multimodal_models() {
        assert!(model_capabilities("gpt-4o").vision);
        assert!(model_capabilities("claude-sonnet-4-5").pdf);
        assert!(!model_capabilities("text-only").vision);
    }

    #[test]
    fn extracts_hard_tool_transport_requirements_from_frozen_definitions() {
        let requirements = tool_transport_requirements_from_body(&json!({
            "tools": [
                {"type": "function", "function": {"name": "lookup", "strict": true}},
                {"type": "custom", "custom": {
                    "name": "shell",
                    "output_schema": {"type": "string"}
                }}
            ]
        }));

        assert!(requirements.function_tools);
        assert!(requirements.custom_freeform_tools);
        assert!(requirements.strict_function_schema);
        assert!(requirements.tool_output_schema);
    }

    #[test]
    fn null_output_schema_is_absent_but_a_schema_is_required() {
        let absent = tool_transport_requirements_from_body(&json!({
            "tools": [{"type": "function", "function": {
                "name": "lookup",
                "output_schema": null
            }}]
        }));
        let required = tool_transport_requirements_from_body(&json!({
            "tools": [{"type": "function", "function": {
                "name": "lookup",
                "output_schema": {"type": "string"}
            }}]
        }));

        assert!(!absent.tool_output_schema);
        assert!(required.tool_output_schema);
    }
}
