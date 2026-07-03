//! Outbound event router: consumes `CoreEvent`s off a broadcast channel and
//! renders them onto every gateway surface bound to the originating session.
//! Ported from TS `packages/core/src/core/router.ts`, with one deliberate
//! delta: TS serializes renders per-session via promise chains, while this
//! router processes the broadcast stream on a single task (see task-3
//! report). Render errors are swallowed in both, matching TS parity.

use crate::domain::CoreEvent;
use crate::gateway::{Gateway, MessageRef};
use crate::store::Store;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::broadcast;

/// Split `s` into <=1900-char slices for gateways with per-message size
/// limits. Empty input yields a single `"(done)"` placeholder so callers
/// always have something to post.
pub fn chunk(s: &str) -> Vec<String> {
    if s.is_empty() {
        return vec!["(done)".to_string()];
    }
    s.chars()
        .collect::<Vec<char>>()
        .chunks(1900)
        .map(|c| c.iter().collect())
        .collect()
}

/// Per-session render state: the coalesced assistant-text buffer, and the
/// cached status `MessageRef` per `"{gateway}:{conversation_id}"` surface key
/// (so a session's status line is posted once, then edited in place).
#[derive(Default)]
struct SessionRenderState {
    buffer: String,
    status_refs: HashMap<String, MessageRef>,
}

/// Renders `CoreEvent`s onto every gateway surface bound to their session.
pub struct Router {
    store: Arc<Store>,
    gateways: HashMap<String, Arc<dyn Gateway>>,
    state: HashMap<String, SessionRenderState>,
}

impl Router {
    pub fn new(store: Arc<Store>, gateways: Vec<Arc<dyn Gateway>>) -> Self {
        let gateways = gateways
            .into_iter()
            .map(|g| (g.id().to_string(), g))
            .collect();
        Router {
            store,
            gateways,
            state: HashMap::new(),
        }
    }

    /// Consume the broadcast until it closes (all senders dropped). Intended
    /// to be spawned with `tokio::spawn`. Single-task serialization: render
    /// errors are swallowed (TS parity).
    pub async fn run(mut self, mut rx: broadcast::Receiver<CoreEvent>) {
        loop {
            match rx.recv().await {
                Ok(event) => self.handle_event(event).await,
                Err(broadcast::error::RecvError::Lagged(n)) => {
                    eprintln!("[router] lagged: dropped {n} events");
                    continue;
                }
                Err(broadcast::error::RecvError::Closed) => break,
            }
        }
    }

    async fn handle_event(&mut self, event: CoreEvent) {
        match event {
            CoreEvent::Message {
                session_pk,
                block_type,
                payload,
                ..
            } => match block_type.as_str() {
                "text" => {
                    if let Some(text) = payload.get("text").and_then(|v| v.as_str()) {
                        self.state
                            .entry(session_pk)
                            .or_default()
                            .buffer
                            .push_str(text);
                    }
                }
                "status" => {
                    if let Some(text) = payload.get("summary").and_then(|v| v.as_str()) {
                        let text = text.to_string();
                        self.render_status(&session_pk, &text).await;
                    }
                }
                _ => {}
            },
            CoreEvent::Result { session_pk } => self.render_result(&session_pk).await,
            CoreEvent::Error {
                session_pk,
                message,
            } => self.render_error(&session_pk, &message).await,
            CoreEvent::SessionEnded { session_pk } => {
                self.state.remove(&session_pk);
            }
            CoreEvent::SessionCreated { .. } | CoreEvent::ApprovalRequested { .. } => {}
        }
    }

    async fn render_status(&mut self, session_pk: &str, text: &str) {
        let surfaces = self.store.surfaces(session_pk).await.unwrap_or_default();
        for surface in surfaces {
            let Some(gw) = self.gateways.get(&surface.gateway).cloned() else {
                continue;
            };
            let key = format!("{}:{}", surface.gateway, surface.conversation_id);
            let existing = self
                .state
                .get(session_pk)
                .and_then(|st| st.status_refs.get(&key).cloned());
            if let Some(msg_ref) = existing {
                let _ = gw.edit_status(&msg_ref, text).await;
            } else if let Ok(msg_ref) = gw.post_status(&surface, text).await {
                self.state
                    .entry(session_pk.to_string())
                    .or_default()
                    .status_refs
                    .insert(key, msg_ref);
            }
        }
    }

    async fn render_result(&mut self, session_pk: &str) {
        let buffer = self
            .state
            .get(session_pk)
            .map(|st| st.buffer.clone())
            .unwrap_or_default();
        let chunks = chunk(&buffer);
        let surfaces = self.store.surfaces(session_pk).await.unwrap_or_default();
        for surface in surfaces {
            if let Some(gw) = self.gateways.get(&surface.gateway) {
                let _ = gw.post_result(&surface, &chunks).await;
            }
        }
        self.state.remove(session_pk);
    }

    async fn render_error(&mut self, session_pk: &str, message: &str) {
        let surfaces = self.store.surfaces(session_pk).await.unwrap_or_default();
        for surface in surfaces {
            if let Some(gw) = self.gateways.get(&surface.gateway) {
                let _ = gw.post_error(&surface, message).await;
            }
        }
        self.state.remove(session_pk);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{ApprovalDecision, ApprovalRequest, Surface};
    use async_trait::async_trait;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    #[test]
    fn chunk_empty_input_yields_done_placeholder() {
        assert_eq!(chunk(""), vec!["(done)".to_string()]);
    }

    #[test]
    fn chunk_boundary_at_1900_chars_is_a_single_chunk() {
        let s = "a".repeat(1900);
        let chunks = chunk(&s);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].chars().count(), 1900);
    }

    #[test]
    fn chunk_boundary_at_1901_chars_splits_into_two() {
        let s = "a".repeat(1901);
        let chunks = chunk(&s);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), 1900);
        assert_eq!(chunks[1].chars().count(), 1);
    }

    #[test]
    fn chunk_boundary_at_3800_chars_splits_into_two_equal_chunks() {
        let s = "a".repeat(3800);
        let chunks = chunk(&s);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].chars().count(), 1900);
        assert_eq!(chunks[1].chars().count(), 1900);
    }

    /// Records every call in order; `post_status` hands back incrementing
    /// message ids so `edit_status` targeting can be asserted.
    struct FakeGateway {
        gid: String,
        calls: Mutex<Vec<String>>,
        n: AtomicU64,
    }

    impl FakeGateway {
        fn new(gid: &str) -> Self {
            FakeGateway {
                gid: gid.to_string(),
                calls: Mutex::new(Vec::new()),
                n: AtomicU64::new(0),
            }
        }

        fn calls(&self) -> Vec<String> {
            self.calls.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl Gateway for FakeGateway {
        fn id(&self) -> &str {
            &self.gid
        }
        async fn start(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            Ok(())
        }
        async fn create_workspace(&self, _name: &str) -> anyhow::Result<String> {
            Ok("ws".into())
        }
        async fn create_conversation(
            &self,
            _workspace_id: &str,
            _title: &str,
        ) -> anyhow::Result<String> {
            Ok("conv".into())
        }
        async fn post_status(&self, surface: &Surface, text: &str) -> anyhow::Result<MessageRef> {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            self.calls
                .lock()
                .unwrap()
                .push(format!("post_status:{}:{}", surface.conversation_id, text));
            Ok(MessageRef {
                surface: surface.clone(),
                message_id: format!("m{n}"),
            })
        }
        async fn edit_status(&self, msg: &MessageRef, text: &str) -> anyhow::Result<()> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("edit_status:{}:{}", msg.message_id, text));
            Ok(())
        }
        async fn post_result(&self, surface: &Surface, chunks: &[String]) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!(
                "post_result:{}:{}",
                surface.conversation_id,
                chunks.len()
            ));
            Ok(())
        }
        async fn post_error(&self, surface: &Surface, message: &str) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push(format!(
                "post_error:{}:{}",
                surface.conversation_id, message
            ));
            Ok(())
        }
        async fn request_approval(
            &self,
            _surface: &Surface,
            _req: &ApprovalRequest,
        ) -> anyhow::Result<ApprovalDecision> {
            Ok(ApprovalDecision::AllowOnce)
        }
    }

    async fn test_store() -> Arc<Store> {
        let tmp = tempfile::NamedTempFile::new().unwrap();
        Arc::new(Store::open(tmp.path()).await.unwrap())
    }

    fn text_event(session_pk: &str, seq: i64, text: &str) -> CoreEvent {
        CoreEvent::Message {
            session_pk: session_pk.into(),
            seq,
            role: "assistant".into(),
            block_type: "text".into(),
            payload: serde_json::json!({ "text": text }),
            tool_call_id: None,
            status: None,
            tool_kind: None,
        }
    }

    fn status_event(session_pk: &str, text: &str) -> CoreEvent {
        CoreEvent::Message {
            session_pk: session_pk.into(),
            seq: 1,
            role: "system".into(),
            block_type: "status".into(),
            payload: serde_json::json!({ "summary": text }),
            tool_call_id: None,
            status: None,
            tool_kind: None,
        }
    }

    #[tokio::test]
    async fn text_chunks_buffer_and_flush_as_one_post_result_on_result() {
        let store = test_store().await;
        store.add_surface("fake", "c1", "s1").await.unwrap();
        let gw = Arc::new(FakeGateway::new("fake"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(store.clone(), vec![gw.clone() as Arc<dyn Gateway>]);
        let handle = tokio::spawn(router.run(rx));

        tx.send(text_event("s1", 1, &"a".repeat(1000))).unwrap();
        tx.send(text_event("s1", 2, &"b".repeat(1200))).unwrap();
        tx.send(CoreEvent::Result {
            session_pk: "s1".into(),
        })
        .unwrap();
        drop(tx);
        handle.await.unwrap();

        let calls = gw.calls();
        assert_eq!(calls, vec!["post_result:c1:2".to_string()]);
    }

    #[tokio::test]
    async fn status_posts_once_then_edits() {
        let store = test_store().await;
        store.add_surface("fake", "c1", "s1").await.unwrap();
        let gw = Arc::new(FakeGateway::new("fake"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(store.clone(), vec![gw.clone() as Arc<dyn Gateway>]);
        let handle = tokio::spawn(router.run(rx));

        tx.send(status_event("s1", "working")).unwrap();
        tx.send(status_event("s1", "still working")).unwrap();
        drop(tx);
        handle.await.unwrap();

        assert_eq!(
            gw.calls(),
            vec![
                "post_status:c1:working".to_string(),
                "edit_status:m0:still working".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn status_posts_once_per_surface_across_gateways_with_independent_refs() {
        // Two surfaces on TWO DIFFERENT gateways bound to the same session:
        // each surface must get its own post-then-edit ref, independent of
        // the other surface's.
        let store = test_store().await;
        store.add_surface("fake1", "c1", "s1").await.unwrap();
        store.add_surface("fake2", "c2", "s1").await.unwrap();
        let gw1 = Arc::new(FakeGateway::new("fake1"));
        let gw2 = Arc::new(FakeGateway::new("fake2"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(
            store.clone(),
            vec![
                gw1.clone() as Arc<dyn Gateway>,
                gw2.clone() as Arc<dyn Gateway>,
            ],
        );
        let handle = tokio::spawn(router.run(rx));

        tx.send(status_event("s1", "working")).unwrap();
        tx.send(status_event("s1", "still working")).unwrap();
        drop(tx);
        handle.await.unwrap();

        assert_eq!(
            gw1.calls(),
            vec![
                "post_status:c1:working".to_string(),
                "edit_status:m0:still working".to_string(),
            ]
        );
        assert_eq!(
            gw2.calls(),
            vec![
                "post_status:c2:working".to_string(),
                "edit_status:m0:still working".to_string(),
            ]
        );
    }

    #[tokio::test]
    async fn error_posts_then_a_later_result_renders_done_for_dropped_state() {
        let store = test_store().await;
        store.add_surface("fake", "c1", "s1").await.unwrap();
        let gw = Arc::new(FakeGateway::new("fake"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(store.clone(), vec![gw.clone() as Arc<dyn Gateway>]);
        let handle = tokio::spawn(router.run(rx));

        tx.send(CoreEvent::Error {
            session_pk: "s1".into(),
            message: "boom".into(),
        })
        .unwrap();
        tx.send(CoreEvent::Result {
            session_pk: "s1".into(),
        })
        .unwrap();
        drop(tx);
        handle.await.unwrap();

        assert_eq!(
            gw.calls(),
            vec![
                "post_error:c1:boom".to_string(),
                "post_result:c1:1".to_string(), // chunk("") => ["(done)"]
            ]
        );
    }

    #[tokio::test]
    async fn events_for_sessions_with_no_surfaces_are_ignored() {
        let store = test_store().await;
        // Deliberately no add_surface for "s1".
        let gw = Arc::new(FakeGateway::new("fake"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(store.clone(), vec![gw.clone() as Arc<dyn Gateway>]);
        let handle = tokio::spawn(router.run(rx));

        tx.send(text_event("s1", 1, "hello")).unwrap();
        tx.send(CoreEvent::Result {
            session_pk: "s1".into(),
        })
        .unwrap();
        drop(tx);
        handle.await.unwrap();

        assert!(gw.calls().is_empty());
    }

    #[tokio::test]
    async fn unknown_gateway_id_is_skipped() {
        let store = test_store().await;
        // Surface bound to "other", but the router only knows "fake".
        store.add_surface("other", "c1", "s1").await.unwrap();
        let gw = Arc::new(FakeGateway::new("fake"));
        let (tx, rx) = broadcast::channel(16);
        let router = Router::new(store.clone(), vec![gw.clone() as Arc<dyn Gateway>]);
        let handle = tokio::spawn(router.run(rx));

        tx.send(text_event("s1", 1, "hello")).unwrap();
        tx.send(CoreEvent::Result {
            session_pk: "s1".into(),
        })
        .unwrap();
        drop(tx);
        handle.await.unwrap();

        assert!(gw.calls().is_empty());
    }
}
