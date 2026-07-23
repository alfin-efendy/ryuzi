//! wasm32-only guest glue for the `nvidia` provider component.
//!
//! The glue itself is identical across every OpenAI-format provider, so it is
//! emitted by the shared `ryuzi_openai_format::provider_component!` macro
//! (documented there) rather than hand-copied per component. Everything
//! provider-specific is the three arguments below.
//!
//! # No `Authorization` is ever set here
//! There is no `ryuzi:http` import to set one on: every request goes through
//! `ryuzi:provider-auth`, where the HOST resolves the user's stored NVIDIA NIM key
//! and injects it per the descriptor's `AuthScheme::Bearer`.

ryuzi_openai_format::provider_component!(
    world: "nvidia",
    provider_id: "nvidia",
    config: crate::logic::CONFIG,
);
