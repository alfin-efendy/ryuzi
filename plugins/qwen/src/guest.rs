//! wasm32-only guest glue for the `qwen` provider component.
//!
//! The glue itself is identical across every OpenAI-format OAuth provider, so it
//! is emitted by the shared `ryuzi_openai_format::oauth_provider_component!`
//! macro (documented there) rather than hand-copied. Everything
//! provider-specific is the arguments below: the OAuth profile, the wire config,
//! and the seeded model list — all from `crate::logic`, each pinned to the
//! `qwen` descriptor by that module's tests.
//!
//! # No `Authorization` is ever set here
//! There is no `ryuzi:http` or `ryuzi:provider-auth` import to set one on: the
//! ONLY egress is `ryuzi:oauth`, where the HOST resolves the user's stored Qwen
//! access token for the `qwen` profile and injects it as `Authorization: Bearer
//! …`, stripping any the component set (it sets none). The component never sees
//! the token. Qwen requires no other provider-specific request headers — the
//! native `oauth_upstream_request` qwen arm sets only the bearer — so the guest
//! adds only content-negotiation headers.

ryuzi_openai_format::oauth_provider_component!(
    world: "qwen",
    oauth_profile: crate::logic::OAUTH_PROFILE,
    config: crate::logic::CONFIG,
    seeded_models: crate::logic::SEEDED_MODELS,
);
