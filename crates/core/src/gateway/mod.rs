use crate::domain::{Actor, ApprovalDecision, ApprovalRequest, CoreEvent, Surface};
use crate::registry::Registry;
use async_trait::async_trait;
use std::sync::Arc;

/// A channel/surface: receives inbound user work and renders outbound output.
#[async_trait]
pub trait Gateway: Send + Sync {
    /// Start listening; the gateway pushes inbound work into the hub.
    async fn start(&self, hub: Arc<dyn GatewayHub>) -> anyhow::Result<()>;
    /// Render a core event back onto a surface.
    async fn deliver(&self, surface: &Surface, event: &CoreEvent) -> anyhow::Result<()>;
    /// Ask for tool approval on a surface; resolves to the user's decision.
    async fn request_approval(
        &self,
        surface: &Surface,
        req: &ApprovalRequest,
    ) -> anyhow::Result<ApprovalDecision>;
    async fn shutdown(&self) -> anyhow::Result<()>;
}

/// The core-facing side a gateway calls to drive sessions.
#[async_trait]
pub trait GatewayHub: Send + Sync {
    /// Map an inbound message to a session (start or continue) and run it.
    async fn dispatch(&self, surface: Surface, actor: Actor, prompt: String) -> anyhow::Result<()>;
}

pub trait GatewayFactory: Send + Sync {
    fn create(&self, config: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>>;
}

/// name → `GatewayFactory`. Keyed by `Surface.gateway`.
pub type GatewayRegistry = Registry<dyn GatewayFactory>;

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeGateway;
    #[async_trait]
    impl Gateway for FakeGateway {
        async fn start(&self, _hub: Arc<dyn GatewayHub>) -> anyhow::Result<()> {
            Ok(())
        }
        async fn deliver(&self, _surface: &Surface, _event: &CoreEvent) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _surface: &Surface,
            _req: &ApprovalRequest,
        ) -> anyhow::Result<ApprovalDecision> {
            Ok(ApprovalDecision::AllowOnce)
        }
        async fn shutdown(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }

    struct FakeGatewayFactory;
    impl GatewayFactory for FakeGatewayFactory {
        fn create(&self, _config: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>> {
            Ok(Arc::new(FakeGateway))
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
        };
        let decision = gw.request_approval(&surface, &req).await.unwrap();
        assert_eq!(decision, ApprovalDecision::AllowOnce);
    }
}
