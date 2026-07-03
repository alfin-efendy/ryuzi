pub mod approval;
pub mod attachments;
pub mod connector;
pub mod control;
pub mod daemon;
pub mod daemon_status;
pub mod domain;
pub mod gateway;
pub mod harness;
pub mod integration;
pub mod paths;
pub mod policy;
pub mod registry;
pub mod router;
pub mod settings;
pub mod sidecar;
pub mod store;
pub mod telemetry;
pub mod worktree;

pub use connector::{Connector, ConnectorCtx, ConnectorFactory, ConnectorRegistry};
pub use control::{ControlPlane, ProvisionProjectRequest, ProvisionSettings};
pub use domain::{
    Actor, ApprovalDecision, ApprovalRequest, CoreEvent, McpServerSpec, McpTransport, Message,
    PermMode, Project, Session, SessionStatus, Surface,
};
pub use gateway::{Gateway, GatewayFactory, GatewayRegistry, MessageRef};
pub use harness::acp::{AcpAdapterDescriptor, ClaudeCodeIntegration};
pub use harness::{
    Harness, HarnessFactory, HarnessRegistry, HarnessSession, SessionCtx, TurnPrompt,
};
pub use integration::{Integration, Registries};
pub use registry::Registry;
pub use router::{chunk, ConnectOpts, ConnectOutcome, Router};
pub use store::Store;
