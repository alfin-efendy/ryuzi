//! The LLM stream seam for the native runner.
//!
//! In production this delegates to [`crate::llm_router::client`]; tests inject
//! a scripted implementation via [`LlmStreamFactory`] so the runner can be
//! driven without a network.

use crate::llm_router::client::{self, AnthropicEvent, UpstreamCtx};
use crate::store::Store;
use async_trait::async_trait;
use serde_json::Value;
use std::sync::Arc;
use tokio::sync::mpsc;

/// One provider turn as a stream of Anthropic events.
#[async_trait]
pub trait LlmStream: Send + Sync {
    async fn stream(
        &self,
        body: Value,
    ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<AnthropicEvent>>>;
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
    async fn stream(
        &self,
        body: Value,
    ) -> anyhow::Result<mpsc::Receiver<anyhow::Result<AnthropicEvent>>> {
        client::anthropic_messages_stream(&self.ctx, body).await
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
