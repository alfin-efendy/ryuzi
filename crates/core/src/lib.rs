pub mod agents;
pub mod approval;
pub mod connector;
pub mod control;
pub mod domain;
pub mod fsview;
pub mod gateway;
pub mod gateways;
pub mod harness;
pub mod integration;
pub mod mcp;
pub mod paths;
pub mod policy;
pub mod providers;
pub mod registry;
pub mod scheduler;
pub mod store;
pub mod worktree;

pub use control::ControlPlane;
pub use domain::{
    Actor, ApprovalDecision, ApprovalRequest, CoreEvent, McpServerSpec, McpTransport, Message,
    PermMode, Project, Session, SessionStatus, Surface,
};
pub use connector::{Connector, ConnectorCtx, ConnectorFactory, ConnectorRegistry};
pub use gateway::{Gateway, GatewayFactory, GatewayHub, GatewayRegistry};
pub use harness::acp::{AcpAdapterDescriptor, ClaudeCodeIntegration};
pub use harness::{Harness, HarnessFactory, HarnessRegistry, HarnessSession, SessionCtx};
pub use integration::{Integration, Registries};
pub use registry::Registry;
pub use store::Store;
