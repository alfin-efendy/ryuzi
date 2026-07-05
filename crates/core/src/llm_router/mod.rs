//! Local LLM router: provider registry, credentialed connections, endpoint
//! keys, format translation, and the axum endpoint server.
pub mod client;
pub mod connections;
pub mod keys;
pub mod oauth;
pub mod registry;
pub mod responses;
pub mod server;
pub mod sse;
pub mod translate;
pub mod usage;
