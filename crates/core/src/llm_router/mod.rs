//! Local LLM router: provider registry, credentialed connections, endpoint
//! keys, format translation, and the axum endpoint server.
pub mod capabilities;
pub mod connections;
pub mod keys;
pub mod models;
pub mod oauth;
pub mod quota;
pub mod registry;
pub mod responses;
pub mod routes;
pub mod server;
pub mod sse;
pub mod translate;
pub mod usage;
