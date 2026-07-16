use crate::domain::{ApprovalDecision, ApprovalRequest, Surface};
use crate::registry::Registry;
use crate::router::Router;
use async_trait::async_trait;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

pub mod discord;

/// A reference to a previously-posted message a gateway can edit later (e.g.
/// a status line updated in place). Keeps the originating `Surface` rather
/// than a bare channel id, so an edit can be routed back through the same
/// gateway/conversation that posted it.
#[derive(Debug, Clone, PartialEq)]
pub struct MessageRef {
    pub surface: Surface,
    pub message_id: String,
}

/// The normalized operational state a gateway reports after observing its
/// underlying connection, rather than after a start or stop method returns.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GatewayStatus {
    Connected,
    Offline,
}

impl GatewayStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connected => "connected",
            Self::Offline => "offline",
        }
    }
}

/// Number of status transitions retained for a subscriber that is briefly
/// delayed. This is deliberately bounded: a sustained flapping gateway is
/// backpressured by the daemon's bounded delivery queue rather than creating
/// unbounded tasks or memory use.
pub const GATEWAY_STATUS_EVENT_CAPACITY: usize = 128;

/// A gap-free status subscription: `initial` establishes the non-emitting
/// baseline and `events` carries every subsequent distinct transition in order.
///
/// Publishers must create `events` before reading `initial` while holding the
/// same state lock, so a transition cannot fall between the snapshot and the
/// receiver subscription.
pub struct GatewayStatusSubscription {
    pub initial: GatewayStatus,
    pub events: broadcast::Receiver<GatewayStatus>,
    state: Arc<Mutex<GatewayStatus>>,
}

impl GatewayStatusSubscription {
    /// Discard queued events and atomically establish a fresh baseline after a
    /// lag gap. The publisher holds this same lock while updating state and
    /// sending, so no transition can fall between the new receiver and snapshot.
    pub fn resync(&mut self) -> GatewayStatus {
        let state = self.state.lock().unwrap();
        self.events = self.events.resubscribe();
        *state
    }
}

/// Holds the current status and bounded event publisher for a gateway. Keeping
/// subscription creation and publication under one mutex makes snapshot/event
/// handoff race-free.
pub struct GatewayStatusPublisher {
    state: Arc<Mutex<GatewayStatus>>,
    events: broadcast::Sender<GatewayStatus>,
}

impl GatewayStatusPublisher {
    pub fn new(initial: GatewayStatus) -> Self {
        let (events, _) = broadcast::channel(GATEWAY_STATUS_EVENT_CAPACITY);
        Self {
            state: Arc::new(Mutex::new(initial)),
            events,
        }
    }

    pub fn subscribe(&self) -> GatewayStatusSubscription {
        let state = self.state.lock().unwrap();
        let events = self.events.subscribe();
        GatewayStatusSubscription {
            initial: *state,
            events,
            state: Arc::clone(&self.state),
        }
    }

    /// Publishes a distinct transition after updating the snapshot, preserving
    /// the publisher's chronological event order.
    pub fn publish(&self, next: GatewayStatus) -> bool {
        let mut state = self.state.lock().unwrap();
        if *state == next {
            return false;
        }
        *state = next;
        let _ = self.events.send(next);
        true
    }
}

/// A channel/surface driver: creates workspaces/conversations, renders
/// outbound core output (status/result/error), and asks for tool approval.
/// Inbound handling (start listening for messages, dispatch to sessions) is
/// implemented per-gateway rather than on this trait.
#[async_trait]
pub trait Gateway: Send + Sync {
    /// Stable identifier this gateway is registered under (matches `Surface.gateway`).
    fn id(&self) -> &str;
    /// Start the gateway (e.g. connect to Discord and begin listening).
    async fn start(&self) -> anyhow::Result<()>;
    /// Stop the gateway / tear down any connection.
    async fn stop(&self) -> anyhow::Result<()>;
    /// Create a new workspace (e.g. a Discord guild's project channel group).
    async fn create_workspace(&self, name: &str) -> anyhow::Result<String>;
    /// Create a new conversation/thread within a workspace.
    async fn create_conversation(&self, workspace_id: &str, title: &str) -> anyhow::Result<String>;
    /// Post an ephemeral status message; returns a ref so it can be edited later.
    async fn post_status(&self, surface: &Surface, text: &str) -> anyhow::Result<MessageRef>;
    /// Edit a previously-posted status message in place.
    async fn edit_status(&self, msg: &MessageRef, text: &str) -> anyhow::Result<()>;
    /// Post the final result, pre-chunked to the gateway's message-size limit.
    async fn post_result(&self, surface: &Surface, chunks: &[String]) -> anyhow::Result<()>;
    /// Post an error message.
    async fn post_error(&self, surface: &Surface, message: &str) -> anyhow::Result<()>;
    /// Ask for tool approval on a surface; resolves to the user's decision.
    async fn request_approval(
        &self,
        surface: &Surface,
        req: &ApprovalRequest,
    ) -> anyhow::Result<ApprovalDecision>;

    /// Give this gateway a handle to the outbound [`Router`], once one
    /// exists. Default no-op: most gateways are pure output sinks and never
    /// need it.
    ///
    /// Exists to break a construction-order cycle (Task 6, recorded choice):
    /// `build_daemon` builds every `Gateway` via its `GatewayFactory` BEFORE
    /// the `Router` (the `Router` itself needs the already-built gateway
    /// list — see `router.rs`'s module doc on why a second, inbound-only
    /// `Router` instance is built for this). A gateway whose INBOUND
    /// routing needs a `Router` (Discord's `DiscordGateway`) can't receive
    /// one at construction time, so `build_daemon` calls `set_router` on
    /// every gateway right after building that inbound `Router` instead.
    /// Inbound events arriving at a gateway before its `set_router` is
    /// called are dropped with a warning — see
    /// `gateway::discord::DiscordGateway`.
    fn set_router(&self, _router: Arc<Router>) {}

    /// Subscribe to operational connection changes. Gateways without an
    /// independently observed runtime status retain the default no-op.
    fn subscribe_status(&self) -> Option<GatewayStatusSubscription> {
        None
    }
}

pub trait GatewayFactory: Send + Sync {
    fn create(&self, config: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>>;
}

/// name → `GatewayFactory`. Keyed by `Surface.gateway`.
pub type GatewayRegistry = Registry<dyn GatewayFactory>;

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::Mutex;

    /// Records every call it receives; `post_status` hands back incrementing
    /// message ids so tests can assert on `edit_status` targeting.
    struct FakeGateway {
        id: String,
        calls: Mutex<Vec<String>>,
        n: AtomicU64,
    }

    impl FakeGateway {
        fn new(id: &str) -> Self {
            FakeGateway {
                id: id.to_string(),
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
            &self.id
        }
        async fn start(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("start".to_string());
            Ok(())
        }
        async fn stop(&self) -> anyhow::Result<()> {
            self.calls.lock().unwrap().push("stop".to_string());
            Ok(())
        }
        async fn create_workspace(&self, name: &str) -> anyhow::Result<String> {
            self.calls
                .lock()
                .unwrap()
                .push(format!("create_workspace:{name}"));
            Ok(format!("ws-{name}"))
        }
        async fn create_conversation(
            &self,
            workspace_id: &str,
            title: &str,
        ) -> anyhow::Result<String> {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            self.calls
                .lock()
                .unwrap()
                .push(format!("create_conversation:{workspace_id}:{title}"));
            Ok(format!("conv-{n}"))
        }
        async fn post_status(&self, surface: &Surface, text: &str) -> anyhow::Result<MessageRef> {
            let n = self.n.fetch_add(1, Ordering::SeqCst);
            self.calls
                .lock()
                .unwrap()
                .push(format!("post_status:{}:{}", surface.conversation_id, text));
            Ok(MessageRef {
                surface: surface.clone(),
                message_id: format!("m-{n}"),
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
                chunks.join("|")
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
            surface: &Surface,
            req: &ApprovalRequest,
        ) -> anyhow::Result<ApprovalDecision> {
            self.calls.lock().unwrap().push(format!(
                "request_approval:{}:{}",
                surface.conversation_id, req.tool
            ));
            Ok(ApprovalDecision::AllowOnce)
        }
    }

    struct FakeGatewayFactory;
    impl GatewayFactory for FakeGatewayFactory {
        fn create(&self, _config: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>> {
            Ok(Arc::new(FakeGateway::new("discord")))
        }
    }

    #[test]
    fn registry_resolves_gateway_factory_by_name() {
        let mut reg: GatewayRegistry = GatewayRegistry::new();
        reg.register("discord", Arc::new(FakeGatewayFactory));
        assert!(reg.get("discord").is_some());
        assert!(reg.get("slack").is_none());
        assert_eq!(reg.names(), vec!["discord".to_string()]);
    }

    #[tokio::test]
    async fn factory_produces_a_gateway_that_returns_a_decision() {
        let f = FakeGatewayFactory;
        let gw = f.create(&serde_json::json!({})).unwrap();
        let surface = Surface {
            gateway: "discord".into(),
            conversation_id: "c1".into(),
        };
        let req = ApprovalRequest {
            run_id: "run-1".into(),
            requesting_agent_id: "agent-1".into(),
            requesting_agent_name: "Agent 1".into(),
            request_id: "r1".into(),
            tool: "Bash".into(),
            summary: "ls".into(),
            approver_role_ids: vec![],
            started_by: None,
            timeout_ms: None,
            principal: None,
        };
        let decision = gw.request_approval(&surface, &req).await.unwrap();
        assert_eq!(decision, ApprovalDecision::AllowOnce);
    }

    #[tokio::test]
    async fn gateway_surface_lifecycle_calls_are_recorded_in_order() {
        let gw = FakeGateway::new("discord");
        gw.start().await.unwrap();
        let ws = gw.create_workspace("proj").await.unwrap();
        let conv = gw.create_conversation(&ws, "title").await.unwrap();
        let surface = Surface {
            gateway: "discord".into(),
            conversation_id: conv,
        };
        let r1 = gw.post_status(&surface, "working").await.unwrap();
        gw.edit_status(&r1, "still working").await.unwrap();
        gw.post_result(&surface, &["chunk1".to_string()])
            .await
            .unwrap();
        gw.post_error(&surface, "boom").await.unwrap();
        gw.stop().await.unwrap();

        assert_eq!(
            gw.calls(),
            vec![
                "start".to_string(),
                "create_workspace:proj".to_string(),
                "create_conversation:ws-proj:title".to_string(),
                "post_status:conv-0:working".to_string(),
                "edit_status:m-1:still working".to_string(),
                "post_result:conv-0:chunk1".to_string(),
                "post_error:conv-0:boom".to_string(),
                "stop".to_string(),
            ]
        );
    }
}
