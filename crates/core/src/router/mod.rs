//! Local LLM router: provider registry, credentialed connections, endpoint
//! keys, format translation, and the axum endpoint server.
pub mod connections;
pub mod keys;
pub mod registry;
pub mod sse;
pub mod translate;
