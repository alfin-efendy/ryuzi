//! Lightweight model capability inference for routing.
use serde_json::Value;

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
}
