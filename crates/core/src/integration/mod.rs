use crate::connector::{ConnectorFactory, ConnectorRegistry};
use crate::gateway::{GatewayFactory, GatewayRegistry};
use crate::harness::{HarnessFactory, HarnessRegistry};
use std::sync::Arc;

/// A single integration that can plug into one or more extension axes.
/// e.g. GitHub can be both a gateway (issue → session) and a connector
/// (agent manages issues as a tool). Each `Some` axis registers under `id()`.
pub trait Integration: Send + Sync {
    fn id(&self) -> &str;
    fn harness(&self) -> Option<Arc<dyn HarnessFactory>> {
        None
    }
    fn gateway(&self) -> Option<Arc<dyn GatewayFactory>> {
        None
    }
    fn connector(&self) -> Option<Arc<dyn ConnectorFactory>> {
        None
    }
}

/// The three extension registries, plus installation of integrations.
#[derive(Default)]
pub struct Registries {
    pub harness: HarnessRegistry,
    pub gateway: GatewayRegistry,
    pub connector: ConnectorRegistry,
}

impl Registries {
    pub fn new() -> Self {
        Registries::default()
    }

    /// Install an integration into every axis it participates in.
    pub fn install(&mut self, integ: &dyn Integration) {
        if let Some(h) = integ.harness() {
            self.harness.register(integ.id().to_string(), h);
        }
        if let Some(g) = integ.gateway() {
            self.gateway.register(integ.id().to_string(), g);
        }
        if let Some(c) = integ.connector() {
            self.connector.register(integ.id().to_string(), c);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connector::Connector;
    use crate::domain::McpServerSpec;
    use crate::gateway::{Gateway, GatewayHub};
    use crate::harness::{Harness, HarnessSession, SessionCtx};
    use async_trait::async_trait;

    // ---- minimal fakes for each axis (self-contained to this test module) ----

    struct FakeHarness;
    #[async_trait]
    impl Harness for FakeHarness {
        async fn start_session(&self, _ctx: SessionCtx) -> anyhow::Result<Box<dyn HarnessSession>> {
            anyhow::bail!("not needed in this test")
        }
    }
    struct FakeHarnessFactory;
    impl HarnessFactory for FakeHarnessFactory {
        fn create(&self) -> anyhow::Result<Arc<dyn Harness>> {
            Ok(Arc::new(FakeHarness))
        }
    }

    struct FakeGateway;
    #[async_trait]
    impl Gateway for FakeGateway {
        async fn start(&self, _hub: Arc<dyn GatewayHub>) -> anyhow::Result<()> {
            Ok(())
        }
        async fn deliver(
            &self,
            _s: &crate::domain::Surface,
            _e: &crate::domain::CoreEvent,
        ) -> anyhow::Result<()> {
            Ok(())
        }
        async fn request_approval(
            &self,
            _s: &crate::domain::Surface,
            _r: &crate::domain::ApprovalRequest,
        ) -> anyhow::Result<crate::domain::ApprovalDecision> {
            Ok(crate::domain::ApprovalDecision::Cancel)
        }
        async fn shutdown(&self) -> anyhow::Result<()> {
            Ok(())
        }
    }
    struct FakeGatewayFactory;
    impl GatewayFactory for FakeGatewayFactory {
        fn create(&self, _c: &serde_json::Value) -> anyhow::Result<Arc<dyn Gateway>> {
            Ok(Arc::new(FakeGateway))
        }
    }

    struct FakeConnector;
    #[async_trait]
    impl Connector for FakeConnector {
        async fn mcp_servers(
            &self,
            _ctx: &crate::connector::ConnectorCtx,
        ) -> anyhow::Result<Vec<McpServerSpec>> {
            Ok(vec![])
        }
    }
    struct FakeConnectorFactory;
    impl ConnectorFactory for FakeConnectorFactory {
        fn create(&self, _c: &serde_json::Value) -> anyhow::Result<Arc<dyn Connector>> {
            Ok(Arc::new(FakeConnector))
        }
    }

    // ---- integrations under test ----

    struct HarnessOnly; // like claude-code
    impl Integration for HarnessOnly {
        fn id(&self) -> &str {
            "claude-code"
        }
        fn harness(&self) -> Option<Arc<dyn HarnessFactory>> {
            Some(Arc::new(FakeHarnessFactory))
        }
    }

    struct DualMode; // like github: gateway + connector
    impl Integration for DualMode {
        fn id(&self) -> &str {
            "github"
        }
        fn gateway(&self) -> Option<Arc<dyn GatewayFactory>> {
            Some(Arc::new(FakeGatewayFactory))
        }
        fn connector(&self) -> Option<Arc<dyn ConnectorFactory>> {
            Some(Arc::new(FakeConnectorFactory))
        }
    }

    #[test]
    fn install_routes_each_integration_into_only_its_axes() {
        let mut regs = Registries::new();
        regs.install(&HarnessOnly);
        regs.install(&DualMode);

        // claude-code → harness only
        assert!(regs.harness.get("claude-code").is_some());
        assert!(regs.gateway.get("claude-code").is_none());
        assert!(regs.connector.get("claude-code").is_none());

        // github → gateway + connector (dual-mode), NOT harness
        assert!(regs.harness.get("github").is_none());
        assert!(regs.gateway.get("github").is_some());
        assert!(regs.connector.get("github").is_some());

        assert_eq!(regs.harness.names(), vec!["claude-code".to_string()]);
        assert_eq!(regs.gateway.names(), vec!["github".to_string()]);
        assert_eq!(regs.connector.names(), vec!["github".to_string()]);
    }
}
