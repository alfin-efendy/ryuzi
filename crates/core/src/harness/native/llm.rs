//! The LLM stream seam for the native runner.
//!
//! In production this delegates to [`crate::llm_router::client`]; tests inject
//! a scripted implementation via [`LlmStreamFactory`] so the runner can be
//! driven without a network.

use crate::llm_router::client::{self, MessageStreamEvent, UpstreamCtx};
use crate::llm_router::model_effort::TurnEffortPolicy;
use crate::llm_router::provenance::{LlmRequest, LlmRequestMetadata, RoutedStream};
use crate::store::Store;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;

/// One provider turn as a stream of Anthropic events.
#[async_trait]
pub trait LlmStream: Send + Sync {
    async fn stream(&self, request: LlmRequest) -> anyhow::Result<RoutedStream>;
}

/// Builds an [`LlmStream`] for a session, given its store. Kept separate from
/// `LlmStream` so the harness can be constructed once and produce a fresh
/// stream per session (and so tests can inject scripted streams).
pub trait LlmStreamFactory: Send + Sync {
    fn create(&self, store: Arc<Store>) -> Arc<dyn LlmStream>;
}

/// Production stream over the in-process router client.
pub struct RouterLlmStream {
    ctx: UpstreamCtx,
}

#[async_trait]
impl LlmStream for RouterLlmStream {
    async fn stream(&self, request: LlmRequest) -> anyhow::Result<RoutedStream> {
        client::anthropic_messages_stream(&self.ctx, request.body, &request.metadata.effort_policy)
            .await
    }
}

/// Factory for [`RouterLlmStream`].
pub struct RouterLlmStreamFactory;

impl LlmStreamFactory for RouterLlmStreamFactory {
    fn create(&self, store: Arc<Store>) -> Arc<dyn LlmStream> {
        Arc::new(RouterLlmStream {
            ctx: UpstreamCtx::new(store),
        })
    }
}

/// Resolve the model for a secondary (auxiliary) call — session-title
/// generation, context compaction, orchestrator goal-decompose — from the
/// raw-KV setting `auxiliary.<task>.model`. Falls back to `fallback` (the
/// session/default model) when the setting is unset, unreadable, or blank.
pub async fn aux_model(store: &Store, task: &str, fallback: &str) -> String {
    let key = format!("auxiliary.{task}.model");
    match store.get_setting(&key).await {
        Ok(Some(value)) if !value.trim().is_empty() => value.trim().to_string(),
        _ => fallback.to_string(),
    }
}

/// Stream a request and concatenate its text deltas. A stream `Error` event
/// or transport error becomes `Err`. Shared by title generation and
/// compaction summarization.
pub async fn collect_text(
    llm: &Arc<dyn LlmStream>,
    body: Value,
    effort_policy: Arc<TurnEffortPolicy>,
) -> anyhow::Result<String> {
    let RoutedStream { mut events, .. } = llm
        .stream(LlmRequest {
            body,
            metadata: LlmRequestMetadata {
                effort_policy,
                observation: None,
            },
        })
        .await?;
    let mut out = String::new();
    while let Some(item) = events.recv().await {
        let ev = item?;
        match MessageStreamEvent::from_event(&ev) {
            Some(MessageStreamEvent::TextDelta { text, .. }) => out.push_str(&text),
            Some(MessageStreamEvent::Error(msg)) => anyhow::bail!("{msg}"),
            _ => {}
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::harness::native::runner::testutil::{message_stop, text_delta, ScriptedLlm};

    fn policy() -> Arc<TurnEffortPolicy> {
        Arc::new(TurnEffortPolicy {
            requested_model: "test/model".into(),
            caller_override: None,
            route_targets: Default::default(),
            configured: Default::default(),
            surfaces: Default::default(),
        })
    }

    #[tokio::test]
    async fn collect_text_concatenates_deltas_and_propagates_errors() {
        let llm: Arc<dyn LlmStream> = Arc::new(ScriptedLlm::new(vec![
            vec![text_delta("a"), text_delta("b"), message_stop()],
            vec![(
                "error".to_string(),
                serde_json::json!({"type":"error","error":{"message":"boom"}}),
            )],
        ]));
        assert_eq!(
            collect_text(&llm, serde_json::json!({"stream": true}), policy(),)
                .await
                .unwrap(),
            "ab"
        );
        let err = collect_text(&llm, serde_json::json!({"stream": true}), policy())
            .await
            .unwrap_err();
        assert!(err.to_string().contains("boom"));
    }

    #[tokio::test]
    async fn aux_model_prefers_setting_then_falls_back() {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        let store = Store::open(tmp.path()).await.unwrap();
        // unset → falls back to the provided default
        assert_eq!(aux_model(&store, "title", "sess/model").await, "sess/model");
        store
            .set_setting(
                crate::domain::WriteOrigin::User,
                "auxiliary.title.model",
                "cheap/haiku",
            )
            .await
            .unwrap();
        assert_eq!(
            aux_model(&store, "title", "sess/model").await,
            "cheap/haiku"
        );
    }
}
