//! Local LLM router: provider registry, credentialed connections, endpoint
//! keys, format translation, and the axum endpoint server.
pub mod aws_stream;
pub mod capabilities;
pub mod claude_cloak;
pub mod client;
pub mod codex;
pub mod connections;
pub mod installed;
pub mod keys;
pub mod kiro;
pub mod mimo;
pub mod model_capabilities;
pub mod model_effort;
pub mod model_meta;
pub mod models;
pub mod oauth;
pub mod probe;
pub mod provenance;
pub mod quota;
pub mod registry;
pub mod responses;
pub mod routes;
pub mod secrets;
pub mod server;
pub mod sse;
pub mod translate;
pub mod usage;
