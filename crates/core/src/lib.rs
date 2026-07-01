pub mod approval;
pub mod control;
pub mod domain;
pub mod gateway;
pub mod harness;
pub mod paths;
pub mod policy;
pub mod registry;
pub mod runtime;
pub mod store;
pub mod worktree;

pub use control::ControlPlane;
pub use domain::{
    Actor, AgentEvent, ApprovalDecision, ApprovalRequest, CoreEvent, McpServerSpec, McpTransport,
    Message, PermMode, Project, Session, SessionStatus, Surface,
};
pub use gateway::{Gateway, GatewayFactory, GatewayHub, GatewayRegistry};
pub use harness::{Harness, HarnessFactory, HarnessRegistry, HarnessSession, SessionCtx};
pub use registry::Registry;
pub use store::Store;
