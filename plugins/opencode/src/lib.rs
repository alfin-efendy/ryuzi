//! First-party OpenCode Zen free-tier provider component.
//!
//! Exports `ryuzi:provider/provider@0.1.0` and ports OpenCode's free-tier wire
//! contract (`llm_router::registry`'s `opencode-free` descriptor +
//! `client::apply_provider_request_headers`): base `https://opencode.ai/zen/v1`,
//! `Authorization: Bearer public` + `x-opencode-client: desktop`, and NO
//! bootstrap step. All network I/O goes through the host `ryuzi:http/http`
//! capability; the static bearer is forwarded because this is a VERIFIED
//! first-party bundle (see `capabilities::http` self-auth).
//!
//! Like the MiMo component, all wire-protocol logic lives in the pure [`logic`]
//! module (native `cargo test`) and the wasm-gated `guest` module is thin
//! effect/mapping glue. No storage capability is needed: there is nothing to
//! cache (no minted token, no device identity).

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod guest;
