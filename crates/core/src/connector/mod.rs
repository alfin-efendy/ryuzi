use crate::domain::McpServerSpec;
use crate::registry::Registry;
use async_trait::async_trait;
use std::path::PathBuf;
use std::sync::Arc;

/// Context for resolving a connector's MCP servers.
pub struct ConnectorCtx {
    pub project_id: String,
    pub work_dir: PathBuf,
}

/// A service the agent uses as tools, exposed as MCP server(s) attached to a session.
#[async_trait]
pub trait Connector: Send + Sync {
    /// The MCP servers this connector contributes for a project/session.
    async fn mcp_servers(&self, ctx: &ConnectorCtx) -> anyhow::Result<Vec<McpServerSpec>>;
    /// Optional: run any auth/OAuth needed before the servers are usable.
    async fn ensure_auth(&self, _ctx: &ConnectorCtx) -> anyhow::Result<()> {
        Ok(())
    }
}

pub trait ConnectorFactory: Send + Sync {
    fn create(&self, config: &serde_json::Value) -> anyhow::Result<Arc<dyn Connector>>;
}

/// name → `ConnectorFactory`.
pub type ConnectorRegistry = Registry<dyn ConnectorFactory>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::McpTransport;

    struct FakeConnector;
    #[async_trait]
    impl Connector for FakeConnector {
        async fn mcp_servers(&self, _ctx: &ConnectorCtx) -> anyhow::Result<Vec<McpServerSpec>> {
            Ok(vec![McpServerSpec {
                name: "notion".into(),
                transport: McpTransport::Stdio {
                    command: "notion-mcp".into(),
                    args: vec![],
                    env: vec![],
                },
            }])
        }
    }

    struct FakeConnectorFactory;
    impl ConnectorFactory for FakeConnectorFactory {
        fn create(&self, _config: &serde_json::Value) -> anyhow::Result<Arc<dyn Connector>> {
            Ok(Arc::new(FakeConnector))
        }
    }

    #[test]
    fn registry_resolves_connector_factory_by_name() {
        let mut reg: ConnectorRegistry = ConnectorRegistry::new();
        reg.register("notion", Arc::new(FakeConnectorFactory));
        assert!(reg.get("notion").is_some());
        assert_eq!(reg.names(), vec!["notion".to_string()]);
    }

    #[tokio::test]
    async fn connector_yields_mcp_servers_and_default_auth_is_ok() {
        let f = FakeConnectorFactory;
        let c = f.create(&serde_json::json!({})).unwrap();
        let ctx = ConnectorCtx {
            project_id: "p1".into(),
            work_dir: PathBuf::from("/tmp"),
        };
        c.ensure_auth(&ctx).await.unwrap(); // default impl returns Ok
        let servers = c.mcp_servers(&ctx).await.unwrap();
        assert_eq!(servers.len(), 1);
        assert_eq!(servers[0].name, "notion");
    }
}
