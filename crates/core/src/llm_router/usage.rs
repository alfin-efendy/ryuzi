//! Usage capture for the local router: extract token counts from upstream
//! responses and record request_log/usage_daily rows (best-effort).
use crate::store::{Store, UsageRecord};
use serde_json::Value;
use std::sync::Arc;

pub const PRUNE_AFTER_MS: i64 = 30 * 24 * 60 * 60 * 1000;

#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Usage {
    pub input: i64,
    pub output: i64,
}

pub fn usage_from_openai(v: &Value) -> Usage {
    Usage {
        input: v["usage"]["prompt_tokens"].as_i64().unwrap_or(0),
        output: v["usage"]["completion_tokens"].as_i64().unwrap_or(0),
    }
}

pub fn usage_from_anthropic(v: &Value) -> Usage {
    Usage {
        input: v["usage"]["input_tokens"].as_i64().unwrap_or(0),
        output: v["usage"]["output_tokens"].as_i64().unwrap_or(0),
    }
}

/// Spawn a detached task that writes one request_log/usage_daily row.
/// Best-effort: failures are logged (no secrets) and never propagate back to
/// the served request.
#[allow(clippy::too_many_arguments)]
pub fn record(
    store: &Arc<Store>,
    connection_id: &str,
    provider: &str,
    model: &str,
    client_format: &str,
    usage: Usage,
    status: u16,
    started_ms: i64,
    error: Option<String>,
) {
    let store = store.clone();
    let rec = UsageRecord {
        connection_id: connection_id.to_string(),
        provider: provider.to_string(),
        model: model.to_string(),
        client_format: client_format.to_string(),
        input_tokens: usage.input,
        output_tokens: usage.output,
        status_code: status as i64,
        duration_ms: crate::paths::now_ms() - started_ms,
        error,
        ts: crate::paths::now_ms(),
    };
    tokio::spawn(async move {
        if let Err(e) = store.record_request(rec).await {
            eprintln!("[llm_router] usage record failed: {e}");
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn extracts_openai_and_anthropic_usage() {
        assert_eq!(
            usage_from_openai(&json!({"usage": {"prompt_tokens": 10, "completion_tokens": 5}})),
            Usage {
                input: 10,
                output: 5
            }
        );
        assert_eq!(
            usage_from_anthropic(&json!({"usage": {"input_tokens": 7, "output_tokens": 3}})),
            Usage {
                input: 7,
                output: 3
            }
        );
        assert_eq!(usage_from_openai(&json!({})), Usage::default());
    }
}
