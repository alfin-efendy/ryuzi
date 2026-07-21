//! First-party GitHub connector component.
//!
//! Exports `ryuzi:connector/connector@0.1.0` (the Task 9 adapter's contract:
//! `list-tools` + `invoke`) and speaks the GitHub REST/GraphQL API behind the
//! host-managed `ryuzi:oauth/oauth` capability — the host injects the bearer,
//! so this component never sees an access or refresh token and never drives
//! Device Flow / PKCE (Cockpit + the host own that; see
//! `crates/core/src/plugins/capabilities/oauth.rs`).
//!
//! # Architecture: pure `logic` vs. wasm `guest`
//! Every decision that does not need a live host — which HTTP request each
//! tool builds, how each response is parsed, and the confirmation gate that
//! refuses an unconfirmed mutation *before any request is built* — lives in
//! the [`logic`] module as pure functions over plain Rust types, so it is
//! exercised entirely by native `cargo test` without a wasm host. The `guest`
//! module (compiled only for `wasm32`) is thin glue: it maps the WIT
//! `tool-call` into [`logic`]'s plain types, hands the planned request to
//! `oauth.authorized-request("github", ..)`, and maps the response back into
//! the WIT `tool-result`.
//!
//! # Approval model
//! There is no approval field in the connector ABI (it stays `0.1.0`). Instead,
//! every mutating tool takes a required boolean `confirm` argument; the pure
//! logic returns [`logic::ToolError::ConfirmationRequired`] (producing NO HTTP
//! request) whenever a mutating tool is called without `confirm=true`. This is
//! a component-internal guard that stacks on top of the host's own generic
//! per-tool approval prompt.

pub mod logic;

#[cfg(target_arch = "wasm32")]
mod guest;
