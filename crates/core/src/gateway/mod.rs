use crate::domain::{ApprovalDecision, ApprovalRequest, Surface};
use crate::registry::Registry;
use async_trait::async_trait;
use std::sync::Arc;

pub mod discord;

/// A reference to a previously-posted message a gateway can edit later (e.g.
/// a status line updated in place). Mirrors the TS `MessageRef`
/// (`packages/core/src/gateways/types.ts`), which keeps the originating
/// `Surface` rather than a bare channel id — this struct does the same (see
/// task-3 report for the naming check against TS).
#[derive(Debug, Clone, PartialEq)]
pub struct MessageRef {
    pub surface: Surface,
    pub message_id: String,
}

/// A channel/surface driver: creates workspaces/conversations, renders
/// outbound core output (status/result/error), and asks for tool approval.
/// Ported from the retired TS `Gateway` interface
/// (`packages/core/src/gateways/types.ts`). Inbound methods (start listening
/// for messages, dispatch to sessions) are added per-gateway in 4D-b.
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
            request_id: "r1".into(),
            tool: "Bash".into(),
            summary: "ls".into(),
            approver_role_ids: vec![],
            started_by: None,
            timeout_ms: None,
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
